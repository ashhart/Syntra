FROM rust:1.87-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -s /bin/false syntra
RUN mkdir -p /var/lib/syntra && chown syntra:syntra /var/lib/syntra
COPY --from=builder /app/target/release/syntra /usr/local/bin/syntra
USER syntra
EXPOSE 8787
ENTRYPOINT ["syntra"]
CMD ["serve", "--addr", "0.0.0.0:8787", "--store", "/var/lib/syntra"]
