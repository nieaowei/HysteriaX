FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY crates/hysteriax-server/Cargo.toml crates/hysteriax-server/Cargo.toml
COPY crates/hysteriax-server/migrations crates/hysteriax-server/migrations
COPY crates/hysteriax-server/src crates/hysteriax-server/src
COPY openapi/openapi.yaml openapi/openapi.yaml
RUN cargo build --release -p hysteriax-server

FROM debian:bookworm-slim
ARG HYSTERIAX_VERSION=dev
LABEL org.opencontainers.image.title="HysteriaX" \
      org.opencontainers.image.version="${HYSTERIAX_VERSION}" \
      org.opencontainers.image.licenses="MIT"
RUN useradd --system --home-dir /data --shell /usr/sbin/nologin hysteriax \
    && mkdir -p /data && chown hysteriax:hysteriax /data \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/hysteriax-server /usr/local/bin/hysteriax-server
USER hysteriax
ENV DATABASE_URL=sqlite:///data/hysteriax.db?mode=rwc
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/hysteriax-server"]
