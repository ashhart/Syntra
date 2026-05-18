# Build context is the parent directory containing both Lycan/ and Syntra/.
# See docker-compose.yml: context: .., dockerfile: Syntra/Dockerfile.
FROM rust:1.87-bookworm AS builder
WORKDIR /app
COPY Lycan ./Lycan
COPY Syntra ./Syntra
WORKDIR /app/Syntra
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -s /bin/false syntra
RUN mkdir -p /var/lib/syntra && chown syntra:syntra /var/lib/syntra
COPY --from=builder /app/Syntra/target/release/syntra /usr/local/bin/syntra
USER syntra
EXPOSE 8787
ENTRYPOINT ["syntra"]
CMD ["serve", "--addr", "0.0.0.0:8787", "--store", "/var/lib/syntra"]
