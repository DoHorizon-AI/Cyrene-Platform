# Build with the Cyrene-Platform repository root as context.
FROM rust:1.96.1-slim-bookworm AS build

RUN apt-get update \
    && apt-get install --yes --no-install-recommends protobuf-compiler pkg-config libssl-dev perl \
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

RUN cargo build --locked --release -p cy-workspace-fabric --bin cy-workspace-relay-host

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 10001 -g 10001 -m 0700 /var/lib/cyrene/workspace-relay

COPY --from=build /source/target/release/cy-workspace-relay-host /usr/local/bin/cy-workspace-relay-host

ENV CYRENE_WORKSPACE_RELAY_DIRECTORY=/var/lib/cyrene/workspace-relay \
    CYRENE_LOG_LEVEL=info

USER 10001:10001
ENTRYPOINT ["/usr/local/bin/cy-workspace-relay-host"]
