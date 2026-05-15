FROM rust:1.87-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -r -s /bin/false lycan
RUN mkdir -p /var/lib/lycan && chown lycan:lycan /var/lib/lycan
COPY --from=builder /app/target/release/syntra /usr/local/bin/syntra
USER lycan
EXPOSE 8787
ENTRYPOINT ["syntra"]
CMD ["serve", "--addr", "0.0.0.0:8787", "--store", "/var/lib/lycan"]
