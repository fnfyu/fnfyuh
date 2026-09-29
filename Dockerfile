# syntax=docker/dockerfile:1.7

ARG NODE_VERSION=22.22.2
ARG RUST_VERSION=1.85.1

# Use the official Rust image instead of downloading rustup during the build. This
# is more reliable behind corporate/proxied WSL networks and keeps the toolchain
# version visible in the image tag.
FROM node:${NODE_VERSION}-bookworm AS node-toolchain
FROM rust:${RUST_VERSION}-bookworm AS builder
ARG NPM_REGISTRY=https://registry.npmjs.org
ARG CARGO_MIRROR=sparse+https://rsproxy.cn/index/
# BuildKit supplies these proxy args when configured on the Docker daemon; they
# are intentionally not promoted to ENV so proxy credentials are not persisted.
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY

# Bring the pinned Node toolchain into the pinned Rust builder without installing
# either toolchain from an ad-hoc script.
COPY --from=node-toolchain /usr/local/bin/ /usr/local/bin/
COPY --from=node-toolchain /usr/local/lib/node_modules/ /usr/local/lib/node_modules/

ENV CI=true \
    CARGO_NET_RETRY=5 \
    CARGO_HTTP_TIMEOUT=120 \
    CARGO_HTTP_MULTIPLEXING=false

RUN rustc --version \
    && cargo --version \
    && node --version \
    && npm --version

RUN mkdir -p /root/.cargo /usr/local/cargo \
    && { \
         printf '%s\n' '[source.crates-io]' 'replace-with = "mirror"' '[source.mirror]'; \
         printf 'registry = "%s"\n' "${CARGO_MIRROR}"; \
         printf '%s\n' '[net]' 'retry = 5'; \
       } | tee /root/.cargo/config.toml /usr/local/cargo/config.toml >/dev/null

WORKDIR /src
COPY package.json tsconfig.json ./
RUN npm config set registry "${NPM_REGISTRY}" \
    && npm install --ignore-scripts --no-audit --no-fund \
        --fetch-retries=5 --fetch-retry-factor=2 \
        --fetch-retry-mintimeout=2000 --fetch-retry-maxtimeout=60000
COPY sdk ./sdk
COPY apps/cli ./apps/cli
COPY apps/gateway ./apps/gateway
COPY apps/web ./apps/web
COPY apps/ide ./apps/ide
COPY docker-compose.yml ./docker-compose.yml
RUN npm test && npm run build:sdk && npm run web:smoke

COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY crates ./crates
COPY apps/daemon ./apps/daemon
RUN cargo test --workspace
RUN cargo build --release -p harness-daemon

FROM node:${NODE_VERSION}-bookworm-slim AS runtime
ENV HARNESS_DB=/data/events.sqlite \
    NODE_ENV=production

RUN mkdir -p /app /data /workspace \
    && chown -R node:node /app /data /workspace

COPY --from=builder /src/target/release/harnessd /usr/local/bin/harnessd
COPY --from=builder /src/apps/cli /app/apps/cli
COPY --from=builder /src/apps/gateway /app/apps/gateway
COPY --from=builder /src/apps/web /app/apps/web
COPY --from=builder /src/apps/ide /app/apps/ide
COPY --from=builder /src/sdk/src /app/sdk/src
COPY --from=builder /src/node_modules /app/node_modules

WORKDIR /app
USER node
VOLUME ["/data", "/workspace"]
ENTRYPOINT ["harnessd"]
