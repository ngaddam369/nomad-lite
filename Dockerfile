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
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/nomad-lite /app/nomad-lite

EXPOSE 50051 8080

ENTRYPOINT ["/app/nomad-lite"]
