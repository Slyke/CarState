# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS development
ARG RUST_VERSION=1.98.1
RUN rustup toolchain install ${RUST_VERSION} --profile minimal --component clippy,rustfmt && rustup default ${RUST_VERSION}
WORKDIR /app
ENV CARGO_TARGET_DIR=/app/target
CMD ["cargo", "run", "--locked"]

FROM development AS builder
ARG BUILD_HASH=unknown
ARG BUILD_NUMBER
ARG CARSTATE_RELEASE=false
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
ENV BUILD_HASH=${BUILD_HASH} BUILD_NUMBER=${BUILD_NUMBER} CARSTATE_RELEASE=${CARSTATE_RELEASE}
RUN cargo build --locked --release --bin carstate

FROM debian:bookworm-slim AS production
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 carstate && useradd --uid 10001 --gid carstate --no-create-home carstate
WORKDIR /app
COPY --from=builder /app/target/release/carstate /usr/local/bin/carstate
COPY --chmod=644 config/carstate.example.json5 ./config/carstate.json5
COPY --chmod=644 config/errors.json5 ./config/
USER 10001:10001
EXPOSE 3000
STOPSIGNAL SIGTERM
ENTRYPOINT ["carstate"]
