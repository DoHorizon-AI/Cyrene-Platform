# syntax=docker/dockerfile:1.7

# Build from the Cyrene-Platform repository root. Runtime Product data arrives
# through the shared immutable versions volume and is selected by Authority.
FROM rust:1.96.1-slim-bookworm AS build

RUN apt-get update \
    && apt-get install --yes --no-install-recommends make perl protobuf-compiler pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY contracts contracts
COPY kernel kernel
COPY framework framework
COPY runtime runtime
COPY agents agents
COPY adapters adapters
COPY sdk/rust sdk/rust
RUN cargo build --locked --release -p cy-workspace-web-bff --bin cy-workspace-web-bff

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libssl3 util-linux coreutils \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 0 -g 0 -m 0755 /opt/cyrene \
    && install -d -o 0 -g 0 -m 0755 /usr/local/libexec/cyrene \
    && install -d -o 0 -g 0 -m 0555 /var/lib/cyrene-product-bundles/versions

COPY --from=build /source/target/release/cy-workspace-web-bff /usr/local/bin/cy-workspace-web-bff
COPY framework/crates/cy-workspace-web-bff/entrypoint.sh /usr/local/libexec/cyrene/workspace-web-bff-entrypoint
RUN chmod 0555 /usr/local/libexec/cyrene/workspace-web-bff-entrypoint \
    && chmod -R a-w /opt/cyrene

ENV CYRENE_WORKSPACE_PRODUCT_BUNDLE_VERSIONS_ROOT=/var/lib/cyrene-product-bundles/versions
ENV CYRENE_WORKSPACE_WEB_BFF_BIND=0.0.0.0:8080
ENV CYRENE_WORKSPACE_WEB_BFF_AUTHORITY_UDS=/run/cyrene-workspace-authority/bff/bff.sock

EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=45s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8080/readyz || exit 1
ENTRYPOINT ["/usr/local/libexec/cyrene/workspace-web-bff-entrypoint"]
