-- ┌─────────────────────────────────────────────────────────────────────┐
-- │  📄 0001_restricted_device_ca.up.sql                                │
-- │  Schema: cyrene_workspace_device_ca                                  │
-- │  Role: Persist idempotent device issuance and current signed CRL.     │
-- │                                                                     │
-- │  用途：持久化设备幂等签发记录与当前签名 CRL。                         │
-- └─────────────────────────────────────────────────────────────────────┘
-- Restricted local Workspace device CA ledger.
-- The application role is deliberately NOLOGIN; operators grant it to a
-- separate service LOGIN role after provisioning credentials out of band.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_ca_app') THEN
        CREATE ROLE cyrene_workspace_device_ca_app;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'cyrene_workspace_device_ca_reader') THEN
        CREATE ROLE cyrene_workspace_device_ca_reader;
    END IF;
END
$$;

ALTER ROLE cyrene_workspace_device_ca_app
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
ALTER ROLE cyrene_workspace_device_ca_reader
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;

CREATE SCHEMA IF NOT EXISTS cyrene_workspace_device_ca;
REVOKE ALL ON SCHEMA cyrene_workspace_device_ca FROM PUBLIC;
GRANT USAGE ON SCHEMA cyrene_workspace_device_ca TO cyrene_workspace_device_ca_app;
GRANT USAGE ON SCHEMA cyrene_workspace_device_ca TO cyrene_workspace_device_ca_reader;

CREATE TABLE cyrene_workspace_device_ca.issued_certificates (
    authorization_id BYTEA PRIMARY KEY CHECK (octet_length(authorization_id) = 16),
    request_sha256 BYTEA NOT NULL CHECK (octet_length(request_sha256) = 32),
    registration_binding_id UUID NOT NULL,
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256 AND organization_id = btrim(organization_id)),
    workspace_id TEXT NOT NULL CHECK (length(workspace_id) BETWEEN 1 AND 256 AND workspace_id = btrim(workspace_id)),
    device_id TEXT NOT NULL CHECK (length(device_id) BETWEEN 1 AND 256 AND device_id = btrim(device_id)),
    authorization_generation BIGINT NOT NULL CHECK (authorization_generation > 0),
    csr_der BYTEA NOT NULL CHECK (octet_length(csr_der) BETWEEN 1 AND 16384),
    csr_sha256 BYTEA NOT NULL CHECK (octet_length(csr_sha256) = 32),
    spki_sha256 BYTEA NOT NULL CHECK (octet_length(spki_sha256) = 32),
    issued_at_unix_ms BIGINT NOT NULL CHECK (issued_at_unix_ms > 0),
    certificate_der BYTEA NOT NULL CHECK (octet_length(certificate_der) BETWEEN 1 AND 16384),
    certificate_sha256 BYTEA NOT NULL UNIQUE CHECK (octet_length(certificate_sha256) = 32),
    serial_number BYTEA NOT NULL UNIQUE CHECK (octet_length(serial_number) BETWEEN 1 AND 20),
    not_after_unix_ms BIGINT NOT NULL CHECK (not_after_unix_ms > issued_at_unix_ms),
    revoked_at_unix_ms BIGINT CHECK (revoked_at_unix_ms IS NULL OR revoked_at_unix_ms >= issued_at_unix_ms),
    UNIQUE (registration_binding_id, authorization_generation)
);

CREATE TABLE cyrene_workspace_device_ca.current_crl (
    singleton BOOLEAN PRIMARY KEY CHECK (singleton),
    crl_number BIGINT NOT NULL CHECK (crl_number > 0),
    this_update_unix_ms BIGINT NOT NULL CHECK (this_update_unix_ms > 0),
    next_update_unix_ms BIGINT NOT NULL CHECK (next_update_unix_ms > this_update_unix_ms),
    issuer_certificate_sha256 BYTEA NOT NULL CHECK (octet_length(issuer_certificate_sha256) = 32),
    crl_der BYTEA NOT NULL CHECK (octet_length(crl_der) BETWEEN 1 AND 33554432)
);

REVOKE ALL ON cyrene_workspace_device_ca.issued_certificates FROM PUBLIC;
REVOKE ALL ON cyrene_workspace_device_ca.current_crl FROM PUBLIC;
REVOKE ALL ON cyrene_workspace_device_ca.issued_certificates FROM cyrene_workspace_device_ca_reader;
REVOKE ALL ON cyrene_workspace_device_ca.current_crl FROM cyrene_workspace_device_ca_reader;
GRANT SELECT, INSERT ON cyrene_workspace_device_ca.issued_certificates TO cyrene_workspace_device_ca_app;
GRANT UPDATE (revoked_at_unix_ms) ON cyrene_workspace_device_ca.issued_certificates TO cyrene_workspace_device_ca_app;
GRANT SELECT, INSERT ON cyrene_workspace_device_ca.current_crl TO cyrene_workspace_device_ca_app;
GRANT UPDATE (crl_number, this_update_unix_ms, next_update_unix_ms, issuer_certificate_sha256, crl_der)
    ON cyrene_workspace_device_ca.current_crl TO cyrene_workspace_device_ca_app;
GRANT SELECT (certificate_der, certificate_sha256, serial_number, revoked_at_unix_ms)
    ON cyrene_workspace_device_ca.issued_certificates TO cyrene_workspace_device_ca_reader;
GRANT SELECT (singleton, crl_number, this_update_unix_ms, next_update_unix_ms, issuer_certificate_sha256, crl_der)
    ON cyrene_workspace_device_ca.current_crl TO cyrene_workspace_device_ca_reader;
