use actix_governor::{Governor, GovernorConfigBuilder};
use actix_web::{
    get, post,
    web::{Bytes, Data, Path},
    App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use mongodb::{
    bson::{doc, Document},
    options::{ClientOptions, ResolverConfig},
    sync::{Client, Collection},
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json;

#[actix_web::main]
async fn main() {
    dotenv::dotenv().ok();
    let governor_conf = GovernorConfigBuilder::default()
        .per_second(60)
        .burst_size(120)
        .finish()
        .unwrap();

    let _ = HttpServer::new(move || {
        let client_options = ClientOptions::parse_with_resolver_config(
            &std::env::var("DBURL").unwrap(),
            ResolverConfig::cloudflare(),
        )
        .unwrap();
        let client = Client::with_options(client_options).unwrap();
        println!("server running...");
        let collection: Collection<Document> = client.database("quitz").collection("questions");
        let app_data = Data::new(collection);
        App::new()
            .wrap(Governor::new(&governor_conf))
            .app_data(app_data)
            .service(rootrequest)
            .service(get_questions)
            .service(post_question)
            .service(text_question)
            .service(post_answer)
            .service(get_specific_questions)
    })
    .bind(("0.0.0.0", 8080))
    .unwrap()
    .run()
    .await;
}

#[get("/")]
async fn rootrequest(request: HttpRequest) -> impl Responder {
    HttpResponse::Ok().body(format!(
        "I know your ip {:?}",
        request
            .connection_info()
            .realip_remote_addr()
            .unwrap_or("oops I don't know")
    ))
}

const MAX_LEN: usize = 20;

#[post("/ques")]
async fn get_specific_questions(
    body: Bytes,
    collection: Data<Collection<Document>>,
) -> impl Responder {
    let ids = match serde_json::from_slice::<Vec<String>>(&body) {
        Ok(mut val) => {
            val.truncate(MAX_LEN);
            val
        }
        Err(_) => return HttpResponse::BadRequest().finish(),
    };
    let mut cursor = match collection.find(doc! {"_id": {"$in": &ids}}, None) {
        Ok(ques) => ques,
        Err(_) => return HttpResponse::BadRequest().finish(),
    };
    let mut docs: Vec<Document> = Vec::with_capacity(ids.len());
    while let Some(result) = cursor.next() {
        match result {
            Ok(data) => docs.push(data),
            Err(_) => break,
        };
    }
    HttpResponse::Ok().body(serde_json::to_string(&docs).unwrap())
}

#[get("/getques/{len}")]
async fn get_questions(len: Path<usize>, collection: Data<Collection<Document>>) -> impl Responder {
    let len = match len.into_inner() {
        length => {
            if length < MAX_LEN {
                length
            } else {
                MAX_LEN
            }
        }
    };
    let mut cursor = match collection.aggregate(
        [doc! {"$sample" : {"size": u32::try_from(len).unwrap()}}],
        None,
    ) {
        Ok(res) => res,
        Err(_) => return HttpResponse::InternalServerError().finish(),
    };
    let mut data = Vec::with_capacity(len);

    while let Some(doc) = cursor.next() {
        match doc {
            Ok(d) => data.push(d),
            Err(_) => break,
        };
    }
    HttpResponse::Ok().body(serde_json::to_string(&data).unwrap_or("".to_string()))
}

#[post("/postques")]
async fn post_question(body: Bytes, collection: Data<Collection<Document>>) -> impl Responder {
    let server_error = HttpResponse::InternalServerError().finish();
    let bad_req = HttpResponse::BadRequest().finish();

    let mut data = match serde_json::from_slice::<Document>(&body) {
        Ok(d) => d,
        Err(_) => return server_error,
    };
    let choices_type = match data.contains_key("c") {
        true => true,
        false => match data.contains_key("mc") {
            true => false,
            false => return bad_req,
        },
    };
    let len = match match data.get_array(if choices_type { "c" } else { "mc" }) {
        Ok(arr) => arr.len(),
        Err(_) => return bad_req,
    } {
        length => {
            if length > 8 {
                return bad_req;
            } else {
                length
            }
        }
    };
    let id = random_id();
    let _ = data.insert("_id", &id);
    let _ = data.insert("a", vec![0; len]);
    match collection.insert_one(data, None) {
        Ok(_) => HttpResponse::Ok().body(id),
        Err(_) => server_error,
    }
}

#[get("/postques/{ques}")]
async fn text_question(
    question: Path<String>,
    collection: Data<Collection<Document>>,
) -> impl Responder {
    let question = question.into_inner();
    let id = random_id();
    match collection.insert_one(
        doc! {
            "_id": &id,
            "q": question,
            "a": []
        },
        None,
    ) {
        Ok(_) => HttpResponse::Ok().body(id),
        Err(_) => HttpResponse::InternalServerError().finish(),
    }
}

#[get("/postans/{ans}/{id}")]
async fn post_answer(
    params: Path<(String, String)>,
    collection: Data<Collection<Document>>,
) -> impl Responder {
    let server_err = HttpResponse::InternalServerError().finish();
    let ok_req = HttpResponse::Ok().finish();
    let bad_req = HttpResponse::BadRequest().finish();
    if let Ok(Some(question)) = collection.find_one(doc! {"_id": &params.1}, None) {
        if question.contains_key("c") {
            let len = match question.get_array("c") {
                Ok(arr) => arr.len(),
                Err(_) => return server_err,
            };
            if let Ok(a) = params.0.parse::<usize>() {
                if a < len {
                    let update = doc! {
                        "$inc" : {format!("a.{}", a) : 1},
                    };
                    return match collection.update_one(doc! {"_id": &params.1}, update, None) {
                        Ok(_) => ok_req,
                        Err(_) => server_err,
                    };
                } else {
                    return bad_req;
                }
            } else {
                return bad_req;
            }
        } else if question.contains_key("mc") {
            let len = match question.get_array("mc") {
                Ok(l) => l.len(),
                Err(_) => return server_err,
            };
            match params.0.parse::<usize>() {
                Ok(a) => {
                    let max_value = (1 << len) - 1;
                    let mut update = doc! {};
                    for i in 0..len {
                        let cur_choice = 1 << i;
                        if (cur_choice & a) == cur_choice {
                            update.insert(format!("a.{}", i), 1);
                        }
                    }
                    if a > max_value {
                        return bad_req;
                    } else {
                        return match collection.update_one(
                            doc! {"_id": &params.1},
                            doc! {
                            "$inc" : &update },
                            None,
                        ) {
                            Ok(_) => ok_req,
                            Err(_) => server_err,
                        };
                    }
                }
                Err(_) => return bad_req,
            };
        } else {
            return match collection.update_one(
                doc! {"_id": &params.1},
                doc! {"$push": {"a": &params.0} },
                None,
            ) {
                Ok(_) => ok_req,
                Err(_) => server_err,
            };
        }
    } else {
        return server_err;
    }
}

fn random_id() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), 8)
}
