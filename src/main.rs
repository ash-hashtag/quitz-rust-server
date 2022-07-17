use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};

use actix_governor::{Governor, GovernorConfigBuilder};
use actix_web::{
    get, post,
    web::{self, Bytes, Data, Path},
    App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use mongodb::{
    bson::{doc, oid::ObjectId, Document},
    options::{ClientOptions, ResolverConfig},
    Client, Collection,
};
use rand::Rng;
use serde_json;

#[actix_web::main]
async fn main() {
    dotenv::dotenv().ok();
    let governor_conf = GovernorConfigBuilder::default()
        .per_second(60)
        .burst_size(120)
        .finish()
        .unwrap();

    let options = ClientOptions::parse_with_resolver_config(
        &std::env::var("DBURL").unwrap(),
        ResolverConfig::cloudflare(),
    )
    .await
    .unwrap();

    let arc_client = Arc::new(Client::with_options(options).unwrap());
    let cached_docs: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let _ = HttpServer::new(move || {
        // let client_options = ClientOptions::parse_with_resolver_config(
        //     &std::env::var("DBURL").unwrap(),
        //     ResolverConfig::cloudflare(),
        // )
        // .unwrap();
        // let client = Client::with_options(client_options).unwrap();
        let cached_data = Data::new(cached_docs.clone());
        println!("server running...");
        let collection: Collection<Document> = arc_client.database("quitz").collection("questions");
        let app_data = Data::new(collection);
        App::new()
            .wrap(Governor::new(&governor_conf))
            .app_data(web::PayloadConfig::new(1 << 10))
            .app_data(cached_data)
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
    let ids = match serde_json::from_slice::<Vec<ObjectId>>(&body) {
        Ok(mut val) => {
            val.truncate(MAX_LEN);
            val
        }
        _ => return bad_req(),
    };
    let mut cursor = match collection.find(doc! {"_id": {"$in": &ids}}, None).await {
        Ok(ques) => ques,
        _ => return bad_req(),
    };
    let mut docs: Vec<Document> = Vec::with_capacity(ids.len());
    while cursor.advance().await.unwrap_or(false) {
        match cursor.deserialize_current() {
            Ok(data) => docs.push(data),
            _ => break,
        };
    }
    HttpResponse::Ok().body(serde_json::to_string(&docs).unwrap())
}

#[get("/getques/{len}")]
async fn get_questions(
    len: Path<usize>,
    collection: Data<Collection<Document>>,
    prev_req: Data<Arc<Mutex<Option<String>>>>,
) -> impl Responder {
    if rand::thread_rng().gen_bool(1.5 / 2.0) {
        if let Some(body) = prev_req.lock().unwrap().clone() {
            return HttpResponse::Ok().body(body);
        }
    }

    let len = match len.into_inner() {
        length => {
            if length < MAX_LEN {
                length
            } else {
                MAX_LEN
            }
        }
    };
    let mut cursor = match collection
        .aggregate(
            [doc! {"$sample" : {"size": u32::try_from(len).unwrap()}}],
            None,
        )
        .await
    {
        Ok(res) => res,
        _ => return server_err(),
    };
    let mut data = Vec::with_capacity(len);

    while cursor.advance().await.unwrap_or(false) {
        match cursor.deserialize_current() {
            Ok(d) => data.push(d),
            _ => break,
        };
    }
    let data = serde_json::to_string(&data).unwrap();
    *prev_req.lock().unwrap() = Some(data.clone());
    HttpResponse::Ok().body(data)
}

#[post("/postques")]
async fn post_question(body: Bytes, collection: Data<Collection<Document>>) -> impl Responder {
    let mut data = match serde_json::from_slice::<Document>(&body) {
        Ok(d) => d,
        _ => return server_err(),
    };
    if data.contains_key("q") {
        match data.get_str("q") {
            Ok(q) => {
                if q.len() > 300 {
                    return bad_req();
                }
            }
            _ => return bad_req(),
        };
    } else {
        return bad_req();
    }
    let choices_type = match data.contains_key("c") {
        true => true,
        _ => match data.contains_key("mc") {
            true => false,
            _ => return bad_req(),
        },
    };
    let len = match match data.get_array(if choices_type { "c" } else { "mc" }) {
        Ok(arr) => arr.len(),
        _ => return bad_req(),
    } {
        length => {
            if length > 8 {
                return bad_req();
            } else {
                length
            }
        }
    };
    let _ = data.insert("a", vec![u32::MIN; len]);
    match collection.insert_one(data, None).await {
        Ok(res) => HttpResponse::Ok().body(res.inserted_id.to_string()),
        _ => server_err(),
    }
}

#[get("/postques/{ques}")]
async fn text_question(
    question: Path<String>,
    collection: Data<Collection<Document>>,
) -> impl Responder {
    if question.len() > 300 {
        return bad_req();
    }

    let question = question.into_inner();
    match collection
        .insert_one(
            doc! {
                "q": question,
                "a": []
            },
            None,
        )
        .await
    {
        Ok(res) => HttpResponse::Ok().body(res.inserted_id.to_string()),
        _ => server_err(),
    }
}

#[get("/postans/{ans}/{id}")]
async fn post_answer(
    params: Path<(String, String)>,
    collection: Data<Collection<Document>>,
) -> impl Responder {
    let id = match ObjectId::from_str(&params.1) {
        Ok(val) => val,
        _ => return bad_req(),
    };
    if let Ok(Some(question)) = collection.find_one(doc! {"_id": &id}, None).await {
        if question.contains_key("c") {
            let len = match question.get_array("c") {
                Ok(arr) => arr.len(),
                _ => return server_err(),
            };
            if let Ok(a) = params.0.parse::<usize>() {
                if a < len {
                    let update = doc! {
                        "$inc" : {format!("a.{}", a) : u32::MIN + 1},
                    };
                    match collection.update_one(doc! {"_id": &id}, update, None).await {
                        Ok(_) => ok_req(),
                        _ => server_err(),
                    }
                } else {
                    bad_req()
                }
            } else {
                bad_req()
            }
        } else if question.contains_key("mc") {
            let len = match question.get_array("mc") {
                Ok(l) => l.len(),
                Err(_) => return server_err(),
            };
            match params.0.parse::<usize>() {
                Ok(a) => {
                    if a > 0 {
                        let max_value = (1 << len) - 1;
                        let mut update = doc! {};
                        for i in 0..len {
                            let cur_choice = 1 << i;
                            if (cur_choice & a) == cur_choice {
                                update.insert(format!("a.{}", i), 1);
                            }
                        }
                        if a > max_value {
                            bad_req()
                        } else {
                            match collection
                                .update_one(
                                    doc! {"_id": &id},
                                    doc! {
                                    "$inc" : &update },
                                    None,
                                )
                                .await
                            {
                                Ok(_) => ok_req(),
                                _ => server_err(),
                            }
                        }
                    } else {
                        bad_req()
                    }
                }
                _ => bad_req(),
            }
        } else {
            if params.0.len() < 300 {
                match collection
                    .update_one(doc! {"_id": &id}, doc! {"$push": {"a": &params.0} }, None)
                    .await
                {
                    Ok(_) => ok_req(),
                    _ => server_err(),
                }
            } else {
                bad_req()
            }
        }
    } else {
        server_err()
    }
}

fn bad_req() -> HttpResponse {
    HttpResponse::BadRequest().finish()
}

fn server_err() -> HttpResponse {
    HttpResponse::InternalServerError().finish()
}

fn ok_req() -> HttpResponse {
    HttpResponse::Ok().finish()
}
