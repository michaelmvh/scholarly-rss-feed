# ---- Build stage ----
FROM rust:1-slim-bookworm AS builder
WORKDIR /app

# Pre-build dependencies against a stub so they are cached in their own layer
# and only recompiled when Cargo.toml / Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build the actual application.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- One-shot publisher: no Docker daemon or host publishing tools required ----
FROM node:24-bookworm-slim AS publisher
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && find /var/lib/apt/lists -mindepth 1 -delete
WORKDIR /opt/publisher
COPY --from=builder /app/target/release/scholarly-rss-feed /usr/local/bin/scholarly-rss-feed
COPY publisher/ ./
RUN chmod 755 /opt/publisher/git-askpass.sh
COPY feeds.toml /config/feeds.toml
ENV GSRF_CONFIG=/config/feeds.toml
ENTRYPOINT ["node", "/opt/publisher/main.mts"]

# ---- Existing HTTP server remains the default build target ----
FROM debian:bookworm-slim AS server
WORKDIR /app

# TLS trust for provider API calls is provided by rustls, so no
# ca-certificates package is required.
COPY --from=builder /app/target/release/scholarly-rss-feed /usr/local/bin/scholarly-rss-feed

# Bake the feed definitions into the image so the repo is the single source of
# truth: edit feeds.toml, push, and the NAS just pulls the updated image. A host
# mount at /config/feeds.toml can still override this without a rebuild.
COPY feeds.toml /config/feeds.toml

EXPOSE 3005

# Use the baked config by default; a host bind mount can override this path.
ENV GSRF_CONFIG=/config/feeds.toml
ENV GSRF_CACHE_DIR=/cache

# Bind to all interfaces so the feed is reachable outside the container.
CMD ["scholarly-rss-feed", "0.0.0.0:3005"]
