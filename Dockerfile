FROM rust:1.97-bookworm AS build
ARG TARGETARCH
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY crates/hysteriax-server/Cargo.toml crates/hysteriax-server/Cargo.toml
COPY crates/hysteriax-server/migrations crates/hysteriax-server/migrations
COPY crates/hysteriax-server/src crates/hysteriax-server/src
COPY openapi/openapi.yaml openapi/openapi.yaml
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=hysteriax-target-${TARGETARCH},target=/src/target,sharing=locked \
    cargo build --locked --release -p hysteriax-server && cp target/release/hysteriax-server /hysteriax-server

FROM debian:bookworm-slim
ARG HYSTERIAX_VERSION=dev
LABEL org.opencontainers.image.title="HysteriaX" \
      org.opencontainers.image.version="${HYSTERIAX_VERSION}" \
      org.opencontainers.image.licenses="MIT"
RUN useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin hysteriax \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Match the managed-node release; verify the published release checksum.
ARG TARGETARCH
RUN set -eu; case "$TARGETARCH" in \
      arm64) asset=hysteria-linux-arm64; expected=c8dc653c3ba0a28d29a26b8fa52d2086f27c0927afddce95c09965e7174e78b0 ;; \
      amd64) asset=hysteria-linux-amd64; expected=8c7a68a906998b747a0db87586e364f995fbfddb95693ae6e2fdb68a6e920d3e ;; \
      *) exit 1 ;; esac; \
    curl -fsSL "https://github.com/apernet/hysteria/releases/download/app/v2.12.3/$asset" -o /tmp/hysteria; \
    printf '%s  /tmp/hysteria\n' "$expected" | sha256sum -c -; \
    install -m 0755 /tmp/hysteria /usr/local/bin/hysteria; rm /tmp/hysteria
COPY --from=build /hysteriax-server /usr/local/bin/hysteriax-server
USER hysteriax
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/hysteriax-server"]
