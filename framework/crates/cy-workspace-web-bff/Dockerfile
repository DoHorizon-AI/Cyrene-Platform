# syntax=docker/dockerfile:1.7

# Build from the Cyrene-Platform repository root and pass the pinned Product
# bundle as the BuildKit context named `product-contracts`.
FROM rust:1.96.1-slim-bookworm AS build

RUN apt-get update \
    && apt-get install --yes --no-install-recommends make perl protobuf-compiler pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json
COPY contracts contracts
COPY kernel kernel
COPY framework framework
COPY runtime runtime
COPY agents agents
COPY adapters adapters
COPY sdk/rust sdk/rust
COPY --from=product-contracts / /source/product-contracts/

ENV CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_BUILD_ROOT=/source/product-contracts

RUN cargo build --locked --release -p cy-workspace-web-bff --bin cy-workspace-web-bff

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libssl3 util-linux coreutils \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 0 -g 0 -m 0755 /opt/cyrene \
    && install -d -o 0 -g 0 -m 0755 /usr/local/libexec/cyrene

COPY --from=build /source/target/release/cy-workspace-web-bff /usr/local/bin/cy-workspace-web-bff
COPY framework/crates/cy-workspace-web-bff/entrypoint.sh /usr/local/libexec/cyrene/workspace-web-bff-entrypoint
COPY --from=product-contracts --chown=0:0 / /opt/cyrene/product-contracts/

RUN chmod 0555 /usr/local/libexec/cyrene/workspace-web-bff-entrypoint \
    && chmod -R a-w /opt/cyrene/product-contracts

ENV CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT_V2=/opt/cyrene/product-contracts
ENV CYRENE_WORKSPACE_WEB_BFF_BIND=0.0.0.0:8080

EXPOSE 8080
ENTRYPOINT ["/usr/local/libexec/cyrene/workspace-web-bff-entrypoint"]
