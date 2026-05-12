FROM rust:slim AS builder

WORKDIR /usr/src/app
COPY . .

# Install dependencies needed for compilation (e.g. openssl, pkg-config)
RUN apt-get update && apt-get install -y pkg-config libssl-dev build-essential protobuf-compiler

# Build the application
RUN cargo build --release

FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies (e.g., ca-certificates and openssl)
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/webhook-injestor-rust /app/webhook-ingester

EXPOSE 8080

CMD ["./webhook-ingester"]
