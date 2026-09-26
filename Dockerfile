# syntax=docker/dockerfile:1

# Build stage: compile the release binary.
FROM rust:1.98-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Runtime stage: a minimal image that only carries the CLI.
FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home clades
COPY --from=builder /build/target/release/clades /usr/local/bin/clades
USER clades
# Reads a JSON document from stdin, writes the JSON report to stdout.
ENTRYPOINT ["clades"]
