# FROM lukemathwalker/cargo-chef:latest-rust-1.56.0 AS chef
# WORKDIR app

# FROM chef AS planner
# COPY . .
# RUN cargo chef prepare --recipe-path recipe.json

# FROM chef AS builder 
# COPY --from=planner /app/recipe.json recipe.json

# RUN cargo chef cook --release --recipe-path recipe.json

# COPY . .

# RUN cargo build --release --bin app

# FROM debian:buster-slim AS runtime
# COPY --from=builder /app/target/release/app /usr/local/bin
# ENTRYPOINT ["/usr/local/bin/app"]

FROM rust:latest AS builder

COPY . .

RUN cargo build --release

FROM gcr.io/distroless/cc-debian11

COPY --from=builder ./target/release/quitz-rust ./

EXPOSE 8080

CMD ["./quitz-rust"]