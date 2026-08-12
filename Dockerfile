FROM node:22-bookworm-slim AS frontend

WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY --from=frontend /build/frontend/dist/ ./frontend/dist/
RUN cargo build --release --locked

FROM ghcr.io/getzola/zola:v0.22.1 AS zola

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home --shell /usr/sbin/nologin blogger \
    && mkdir /data \
    && chown blogger:blogger /data

COPY --from=builder /build/target/release/blogger /usr/local/bin/blogger
COPY --from=zola /bin/zola /usr/local/bin/zola

WORKDIR /data
USER blogger

EXPOSE 3000 3001
CMD ["blogger"]
