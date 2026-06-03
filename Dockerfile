FROM rust:1.96-bookworm AS builder
WORKDIR /app

COPY . .
RUN cargo build --release -p marsala

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/marsala /usr/local/bin/marsala
COPY marsala.example.toml /app/marsala.example.toml

ENV MARSALA__SERVER__HOST=0.0.0.0
ENV MARSALA__LOGGING__LOG_BODIES=false
ENV MARSALA__LOGGING__CAPTURE_STREAM_CHUNKS=false

EXPOSE 8787

CMD ["marsala", "serve"]
