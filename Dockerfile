FROM rust:1.98-alpine AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM scratch
COPY --from=build /app/target/release/clades /clades
ENTRYPOINT ["/clades"]
