# ==============================================================================
# Multi-Stage Production Dockerfile for Kiro BYOK Single-Server Gateway
# ==============================================================================

# --- Stage 1: Build binary ---
FROM rust:1.96.1-slim-bookworm AS builder

WORKDIR /build

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifest and source crates
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps/portal-ui ./apps/portal-ui

# Build release binary for gateway
RUN CARGO_BUILD_JOBS=1 cargo build --locked --release --package gateway --bin gateway

# --- Stage 2: Runtime image ---
FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user with explicit UID/GID 1000 for predictable volume mount permissions
RUN groupadd -r -g 1000 kiro && useradd -r -u 1000 -g kiro -d /app kiro

# Copy binary from builder
COPY --from=builder /build/target/release/gateway /app/gateway

# Create data directory for local state/cards/snapshots
RUN mkdir -p /app/data && chown -R kiro:kiro /app

USER kiro

ENV HOST=0.0.0.0 \
    PORT=19820 \
    DATA_DIR=/app/data

EXPOSE 19820

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -f http://127.0.0.1:19820/healthz || exit 1

ENTRYPOINT ["/app/gateway"]
