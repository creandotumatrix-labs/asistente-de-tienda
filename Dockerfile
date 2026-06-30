# syntax=docker/dockerfile:1
# Multi-stage build for the `server` binary. Railway auto-detects this Dockerfile
# and uses it instead of Railpack (which assumed a package-named binary).

# ---- build ----
FROM rust:1.96-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin server

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/server /usr/local/bin/server
# Seed data + white-label config are read at runtime (relative paths).
COPY config ./config
COPY data ./data
ENV PORT=8080
EXPOSE 8080
CMD ["server"]
