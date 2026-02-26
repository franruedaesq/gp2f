# GP2F Server – Production Dockerfile
#
# Phase 10 requirement 6: Docker image for the GP2F server.
#
# Multi-stage build:
#   1. builder  – compiles the Rust binary with all features
#   2. runtime  – minimal distroless image (no shell, no package manager)
#
# Build:
#   docker build -t gp2f/server:latest .
#
# Run:
#   docker run -e RUST_LOG=info -p 3000:3000 gp2f/server:latest

# ── stage 1: build ────────────────────────────────────────────────────────────
FROM rust:1.78-slim-bookworm AS builder

WORKDIR /workspace

# Cache dependencies before copying source.
COPY Cargo.toml Cargo.lock ./
COPY policy-core/Cargo.toml policy-core/
COPY server/Cargo.toml server/
COPY cli/Cargo.toml cli/

# Create stub src files so cargo can resolve the workspace without full source.
RUN mkdir -p policy-core/src server/src cli/src \
    && echo "fn main(){}" > server/src/main.rs \
    && echo "" > policy-core/src/lib.rs \
    && echo "fn main(){}" > cli/src/main.rs \
    && cargo fetch

# Now copy the full source and build the server binary.
COPY . .
RUN cargo build --release -p gp2f-server --features redis-broadcast

# ── stage 2: runtime ─────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12:latest

WORKDIR /app

# Copy the compiled binary from the builder stage.
COPY --from=builder /workspace/target/release/gp2f-server /app/gp2f-server

# Non-root user (UID 65534 = nobody in distroless).
USER 65534

EXPOSE 3000

ENV RUST_LOG=info

ENTRYPOINT ["/app/gp2f-server"]
