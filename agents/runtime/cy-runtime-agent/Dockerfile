# Build from the Platform repository root. Consumers add their own workload and
# publish the resulting task image by digest; this base is not a training image.
FROM rust:1.96.1-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /source
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY contracts contracts
COPY kernel kernel
COPY framework framework
COPY runtime runtime
COPY agents agents
COPY adapters adapters
COPY sdk/rust sdk/rust
RUN cargo build --locked --release -p cy-runtime-agent --bin cy-runtime-agent

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/cyrene/runtime-agent /var/lib/cyrene/artifacts /run/cyrene \
    && chown -R 10001:10001 /var/lib/cyrene
COPY --from=build /source/target/release/cy-runtime-agent /usr/local/bin/cy-runtime-agent
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/cy-runtime-agent"]
