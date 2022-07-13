FROM rust:latest AS builder

COPY . .

RUN cargo build --release

FROM gcr.io/distroless/cc-debian11

COPY --from=builder ./target/release/quitz-rust ./

EXPOSE 8080

CMD ["./quitz-rust"]