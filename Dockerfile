# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build

WORKDIR /build
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# The project's toolchain file selects the compiler, including the WASM target.
COPY rust-toolchain.toml ./
RUN rustup show active-toolchain \
    && cargo install cargo-leptos --version 0.3.12 --locked

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY public ./public
COPY tests ./tests

RUN cargo fmt --all --check \
    && cargo test --release --locked --features ssr \
    && cargo leptos build --release --cargo-locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates libssl3 tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 minihabits \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin minihabits \
    && install -d -o 10001 -g 10001 /app /data

WORKDIR /app
COPY --from=build /build/target/release/minihabits-rust /usr/local/bin/minihabits-rust
COPY --from=build /build/target/site ./site
# The server loads Leptos settings from this file, with environment overrides.
COPY --from=build /build/Cargo.toml ./Cargo.toml
COPY LICENSE ./LICENSE

ENV LEPTOS_SITE_ADDR=0.0.0.0:5175 \
    LEPTOS_SITE_ROOT=/app/site \
    LEPTOS_ENV=PROD \
    MINIHABITS_DATABASE=/data/minihabits.sqlite3 \
    TZ=UTC

USER 10001:10001
EXPOSE 5175
VOLUME ["/data"]
# The server's graceful-shutdown handler listens for Ctrl+C/SIGINT.
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/local/bin/minihabits-rust"]
