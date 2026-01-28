FROM rust:1.83-slim-bookworm AS builder

# Install protobuf compiler
RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create a dummy src to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs && mkdir -p proto
COPY proto/ proto/
COPY build.rs .

# Build dependencies only (this layer will be cached)
RUN cargo build --release 2>/dev/null || true

# Copy actual source code
COPY src/ src/
COPY examples/ examples/

# Build the actual application
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM docker:27-cli AS docker-cli

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates openssl && rm -rf /var/lib/apt/lists/*
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker

WORKDIR /app

COPY --from=builder /app/target/release/nomad-lite /app/nomad-lite
COPY scripts/gen-test-certs.sh /app/gen-test-certs.sh
RUN chmod +x /app/gen-test-certs.sh

EXPOSE 50051 8080

ENTRYPOINT ["/app/nomad-lite"]
