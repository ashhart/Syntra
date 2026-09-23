# The syntra server image. Build context: the repository root; only
# Cargo.toml, Cargo.lock and src/ enter it (see .dockerignore).
FROM rust:1.94-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked --bin syntra \
    && cp target/release/syntra /usr/local/bin/syntra

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -r -s /bin/false syntra \
    && mkdir -p /var/lib/syntra && chown syntra:syntra /var/lib/syntra
COPY --from=builder /usr/local/bin/syntra /usr/local/bin/syntra
USER syntra
VOLUME ["/var/lib/syntra"]
EXPOSE 8787
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s \
    CMD ["syntra", "health", "--addr", "127.0.0.1:8787"]
ENTRYPOINT ["syntra"]
CMD ["serve", "--addr", "0.0.0.0:8787", "--store", "/var/lib/syntra"]
