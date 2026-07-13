# Build stage
FROM rust:1.85-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock VERSION ./
COPY crates ./crates
COPY docs ./docs
COPY examples ./examples
COPY scripts ./scripts
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release -p grow-cli -p grow-server

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/grow /usr/local/bin/grow
COPY --from=builder /app/target/release/grow-server /usr/local/bin/grow-server
COPY --from=builder /app/docs/man /usr/local/share/man/man1
EXPOSE 7870
ENTRYPOINT ["grow-server"]
CMD ["--help"]