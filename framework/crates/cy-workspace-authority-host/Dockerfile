# syntax=docker/dockerfile:1.7

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
RUN cargo build --locked --release -p cy-workspace-authority-host --bins

FROM ubuntu:22.04 AS runtime
ARG COMPONENT_ID=cy-workspace-authority-host
ARG TARGET_ID=windows-10.0-x86_64-docker-linux
ARG SOURCE_REPOSITORY=https://github.com/DoHorizon-AI/Cyrene-Platform
ARG SOURCE_REVISION=unknown
ARG COMPONENT_VERSION=unknown
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home cyrene \
    && install -d -o 10001 -g 10001 -m 0700 \
        /var/lib/cyrene-workspace-authority \
        /run/cyrene-workspace-authority \
        /run/cyrene-workspace-authority/bff \
    && install -d -o 0 -g 0 -m 0755 /etc/cyrene-workspace-authority
COPY --from=build --chown=10001:10001 /source/target/release/cy-workspace-authority-host /usr/local/bin/cy-workspace-authority-host
COPY --from=build --chown=0:0 /source/target/release/cy-workspace-authority-admin /usr/local/bin/cy-workspace-authority-admin
RUN chmod 0755 /usr/local/bin/cy-workspace-authority-admin
LABEL io.cyrene.component.id=${COMPONENT_ID} \
      io.cyrene.component.target-id=${TARGET_ID} \
      org.opencontainers.image.source=${SOURCE_REPOSITORY} \
      org.opencontainers.image.revision=${SOURCE_REVISION} \
      org.opencontainers.image.version=${COMPONENT_VERSION}
ENV CYRENE_WORKSPACE_AUTHORITY_LISTEN=0.0.0.0:50051 \
    CYRENE_WORKSPACE_AUTHORITY_HEALTH_ADDR=127.0.0.1:8080 \
    CYRENE_WORKSPACE_AUTHORITY_STATE_DIR=/var/lib/cyrene-workspace-authority \
    CYRENE_WORKSPACE_AUTHORITY_BFF_UDS=/run/cyrene-workspace-authority/bff/bff.sock \
    CYRENE_WORKSPACE_AUTHORITY_TRUST_CONFIG=/etc/cyrene-workspace-authority/trust.json \
    CYRENE_WORKSPACE_AUTHORITY_ADMIN_UDS=/run/cyrene-workspace-authority/admin.sock \
    CYRENE_AUTHORITY_BFF_PEER_UID=10001 \
    CYRENE_AUTHORITY_ADMIN_PEER_UID=0 \
    CYRENE_AUTHORITY_AAD_TENANT_ID= \
    CYRENE_AUTHORITY_AAD_AUDIENCE= \
    CYRENE_AUTHORITY_DATABASE_URL_FILE=/run/secrets/cyrene-authority-database-url \
    CYRENE_AUTHORITY_SIGNING_KEY_FILE=/run/secrets/cyrene-authority-signing-key \
    CYRENE_AUTHORITY_SIGNING_KEY_ID= \
    CYRENE_AUTHORITY_TLS_CERT_FILE=/run/secrets/cyrene-authority-tls-cert \
    CYRENE_AUTHORITY_TLS_KEY_FILE=/run/secrets/cyrene-authority-tls-key \
    CYRENE_AUTHORITY_TLS_CLIENT_CA_FILE=/run/secrets/cyrene-authority-client-ca \
    CYRENE_AUTHORITY_INITIAL_ARTIFACT_ID_FILE=/run/secrets/cyrene-authority-initial-artifact-id
USER 10001:10001
EXPOSE 50051 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=90s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8080/readyz || exit 1
ENTRYPOINT ["/usr/local/bin/cy-workspace-authority-host"]
