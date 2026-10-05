//! PostgreSQL regression coverage for terminal Workspace device revocation.
//!
//! This test requires a fresh, isolated database with the Workspace migrations applied. It is
//! ignored by default so ordinary unit-test runs never connect to an ambient database.

use std::time::Duration;

use cy_workspace_control_plane::device_registry::{
    WorkspaceDeviceCertificateIdentity, WorkspaceDeviceKey, WorkspaceDeviceRegistry,
};
use cy_workspace_postgres_storage::{
    DeviceCertificateRegistryActivation, PostgresWorkspaceDeviceRegistry,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_DEVICE_REGISTRY_TEST_DATABASE_URL";
const APPLICATION_NAME: &str = "cyrene-workspace-device-registry";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated PostgreSQL database with all Workspace migrations applied"]
async fn terminal_revoke_serializes_staging_blocks_late_issuance_and_is_retryable() {
    let database_url = std::env::var(DATABASE_URL_ENV).expect("isolated database URL is set");
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .expect("connect to isolated migrated PostgreSQL");
    let app_can_insert: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_device_registry.terminal_device_keys', 'INSERT'\
        )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect runtime role tombstone privileges");
    let app_can_delete: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_device_registry.terminal_device_keys', 'DELETE'\
        )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect runtime role tombstone privileges");
    let owner_can_insert: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(\
            'cyrene_workspace_device_registry_owner', \
            'cyrene_workspace_device_registry.terminal_device_keys', 'INSERT'\
        )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect function owner tombstone privileges");
    let owner_can_delete: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(\
            'cyrene_workspace_device_registry_owner', \
            'cyrene_workspace_device_registry.terminal_device_keys', 'DELETE'\
        )",
    )
    .fetch_one(&pool)
    .await
    .expect("inspect function owner tombstone privileges");
    assert!(!app_can_insert && !app_can_delete);
    assert!(owner_can_insert && !owner_can_delete);
    let runtime_role = format!("cyrene_revocation_test_{}", Uuid::new_v4().simple());
    let runtime_password = Uuid::new_v4().simple().to_string();
    sqlx::query(&format!(
        "CREATE ROLE {runtime_role} LOGIN PASSWORD '{runtime_password}'"
    ))
    .execute(&pool)
    .await
    .expect("create isolated runtime login");
    sqlx::query(&format!(
        "GRANT cyrene_workspace_device_registry_app TO {runtime_role}"
    ))
    .execute(&pool)
    .await
    .expect("grant only the Workspace Registry runtime role");
    let mut registry_url = url::Url::parse(&database_url).expect("valid isolated database URL");
    registry_url
        .set_username(&runtime_role)
        .expect("set runtime login name");
    registry_url
        .set_password(Some(&runtime_password))
        .expect("set runtime login password");
    let registry_url = registry_url.to_string();
    let runtime_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&registry_url)
        .await
        .expect("connect the legacy runtime writer role");
    let registry_stage = std::sync::Arc::new(
        PostgresWorkspaceDeviceRegistry::connect(&registry_url)
            .expect("connect staging registry worker"),
    );
    let registry_revoke = std::sync::Arc::new(
        PostgresWorkspaceDeviceRegistry::connect(&registry_url)
            .expect("connect revocation registry worker"),
    );

    let key = WorkspaceDeviceKey {
        organization_id: "terminal-regression-org".to_owned(),
        workspace_id: "terminal-regression-workspace".to_owned(),
        device_id: Uuid::new_v4().to_string(),
    };
    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.workspace_device_identities \
         (organization_id, workspace_id, device_id, current_authorization_generation) \
         VALUES ($1, $2, $3, 1)",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .execute(&pool)
    .await
    .expect("seed initial Directory identity");
    let first = seed_delivery(&pool, &key, 1).await;
    registry_stage
        .stage_pending_delivery(&first.authorization_id)
        .expect("first generation stages before revocation");
    mark_delivered(&pool, &first, &key).await;
    assert_eq!(
        registry_stage
            .activate_acknowledged_delivery(&first.authorization_id)
            .expect("first generation activation"),
        DeviceCertificateRegistryActivation::Activated
    );

    advance_generation(&pool, &key, 1, 2).await;
    let second = seed_delivery(&pool, &key, 2).await;
    registry_stage
        .stage_pending_delivery(&second.authorization_id)
        .expect("second generation stages before revocation");
    mark_delivered(&pool, &second, &key).await;
    assert_eq!(
        registry_stage
            .activate_acknowledged_delivery(&second.authorization_id)
            .expect("second generation activation"),
        DeviceCertificateRegistryActivation::Activated
    );

    let mut lock_transaction = pool.begin().await.expect("begin key-lock holder");
    sqlx::query("SELECT cyrene_workspace_device_registry.lock_workspace_device_key($1, $2, $3)")
        .bind(&key.organization_id)
        .bind(&key.workspace_id)
        .bind(&key.device_id)
        .execute(&mut *lock_transaction)
        .await
        .expect("hold the shared full-key transaction lock");

    let stage_worker = std::sync::Arc::clone(&registry_stage);
    let stage_id = second.authorization_id;
    let stage = tokio::task::spawn_blocking(move || stage_worker.stage_pending_delivery(&stage_id));
    wait_for_advisory_waiters(&pool, 1).await;

    let revoke_worker = std::sync::Arc::clone(&registry_revoke);
    let revoke_key = key.clone();
    let revoke = tokio::task::spawn_blocking(move || revoke_worker.revoke_device(&revoke_key));
    wait_for_advisory_waiters(&pool, 2).await;
    lock_transaction
        .commit()
        .await
        .expect("release shared key lock");

    stage
        .await
        .expect("staging worker exits")
        .expect("stage precedes revoke");
    let revoked = revoke
        .await
        .expect("revocation worker exits")
        .expect("existing registry key was revoked");
    assert_eq!(
        revoked.authorization_status,
        cy_workspace_control_plane::device_registry::DeviceAuthorizationStatus::Revoked
    );
    assert!(registry_stage
        .is_device_terminal(&key)
        .expect("terminal read-back"));

    let states = sqlx::query_as::<_, (Vec<u8>, String)>(
        "SELECT authorization_id, state \
         FROM cyrene_workspace_device_registry.certificate_records \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
         ORDER BY authorization_generation",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .fetch_all(&pool)
    .await
    .expect("read all device generations");
    assert_eq!(states.len(), 2);
    assert!(states.iter().all(|(_, state)| state == "revoked"));

    assert!(registry_stage
        .stage_pending_delivery(&second.authorization_id)
        .is_err());
    assert_eq!(
        registry_stage
            .activate_acknowledged_delivery(&second.authorization_id)
            .expect("activation after revocation is a typed denial"),
        DeviceCertificateRegistryActivation::Ineligible
    );
    let fingerprint = hex_lower(&second.certificate_sha256);
    assert!(registry_stage
        .find_current_device_certificate_identity(&fingerprint)
        .expect("current-identity lookup")
        .is_none());
    let final_claim = WorkspaceDeviceCertificateIdentity {
        key: key.clone(),
        certificate_fingerprint_sha256: fingerprint,
        authorization_id: Uuid::from_bytes(second.authorization_id),
        registration_binding_id: *second.binding_id.as_bytes(),
        authorization_generation: second.generation,
        csr_sha256: second.csr_sha256,
        spki_sha256: second.spki_sha256,
        serial_number: second.serial_number.clone(),
        not_after_unix_ms: second.not_after_unix_ms,
    };
    assert!(registry_stage
        .acquire_relay_dispatch_fence(&final_claim)
        .is_err());

    advance_generation(&pool, &key, 2, 3).await;
    let late_issue = seed_delivery(&pool, &key, 3).await;
    record_late_ca_issuance(&pool, &key, &late_issue).await;
    assert!(registry_stage
        .stage_pending_delivery(&late_issue.authorization_id)
        .is_err());
    assert_eq!(
        registry_stage
            .activate_acknowledged_delivery(&late_issue.authorization_id)
            .expect("late issuance has no staged record"),
        DeviceCertificateRegistryActivation::NotStaged
    );
    let late_registry_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cyrene_workspace_device_registry.certificate_records \
         WHERE authorization_id = $1",
    )
    .bind(late_issue.authorization_id.to_vec())
    .fetch_one(&pool)
    .await
    .expect("read Registry after late CA issuance");
    assert_eq!(late_registry_count, 0);
    assert!(legacy_pending_insert(&runtime_pool, &late_issue)
        .await
        .is_err());
    assert!(registry_stage
        .is_device_terminal(&key)
        .expect("terminal survives retry"));

    let empty_key = WorkspaceDeviceKey {
        organization_id: "terminal-regression-org".to_owned(),
        workspace_id: "terminal-regression-workspace".to_owned(),
        device_id: Uuid::new_v4().to_string(),
    };
    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.workspace_device_identities \
         (organization_id, workspace_id, device_id, current_authorization_generation) \
         VALUES ($1, $2, $3, 1)",
    )
    .bind(&empty_key.organization_id)
    .bind(&empty_key.workspace_id)
    .bind(&empty_key.device_id)
    .execute(&pool)
    .await
    .expect("seed a bound identity with no issued registry rows");
    let empty_delivery = seed_delivery(&pool, &empty_key, 1).await;
    let mut revoke_first_lock = pool
        .begin()
        .await
        .expect("begin reverse-order key-lock holder");
    sqlx::query("SELECT cyrene_workspace_device_registry.lock_workspace_device_key($1, $2, $3)")
        .bind(&empty_key.organization_id)
        .bind(&empty_key.workspace_id)
        .bind(&empty_key.device_id)
        .execute(&mut *revoke_first_lock)
        .await
        .expect("hold reverse-order device-key lock");
    let revoke_first_worker = std::sync::Arc::clone(&registry_revoke);
    let revoke_first_key = empty_key.clone();
    let revoke_first =
        tokio::task::spawn_blocking(move || revoke_first_worker.revoke_device(&revoke_first_key));
    wait_for_advisory_waiters(&pool, 1).await;
    let stage_after_revoke_worker = std::sync::Arc::clone(&registry_stage);
    let empty_authorization_id = empty_delivery.authorization_id;
    let stage_after_revoke = tokio::task::spawn_blocking(move || {
        stage_after_revoke_worker.stage_pending_delivery(&empty_authorization_id)
    });
    wait_for_advisory_waiters(&pool, 2).await;
    revoke_first_lock
        .commit()
        .await
        .expect("release reverse-order device-key lock");
    assert!(revoke_first
        .await
        .expect("empty-history revoke worker exits")
        .is_err());
    assert!(stage_after_revoke
        .await
        .expect("stage-after-revoke worker exits")
        .is_err());
    assert!(registry_revoke
        .is_device_terminal(&empty_key)
        .expect("CLI may only continue after exact-key terminal read-back"));

    drop(registry_revoke);
    drop(registry_stage);
    runtime_pool.close().await;
    sqlx::query(&format!("DROP ROLE {runtime_role}"))
        .execute(&pool)
        .await
        .expect("remove isolated runtime login");
    pool.close().await;
}

#[derive(Debug)]
struct DeliveryFixture {
    authorization_id: [u8; 16],
    approval_id: [u8; 16],
    binding_id: Uuid,
    generation: u64,
    delivery_id: [u8; 16],
    csr_der: Vec<u8>,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    certificate_der: Vec<u8>,
    certificate_sha256: [u8; 32],
    serial_number: Vec<u8>,
    device_id: String,
    not_after_unix_ms: u64,
    delivery_deadline_unix_ms: u64,
}

async fn seed_delivery(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
    generation: u64,
) -> DeliveryFixture {
    let binding_id = Uuid::new_v4();
    let authorization_id = Uuid::new_v4().into_bytes();
    let approval_id = Uuid::new_v4().into_bytes();
    let delivery_id = Uuid::new_v4().into_bytes();
    let csr_der = vec![0x30, generation as u8, 0x01];
    let csr_sha256 = sha256(&csr_der);
    let spki_sha256 = [generation as u8 + 8; 32];
    let mut certificate_der = vec![0x30, generation as u8, 0x02, 0x03];
    certificate_der.extend_from_slice(Uuid::new_v4().as_bytes());
    let certificate_sha256 = sha256(&certificate_der);
    let mut serial_number = vec![generation as u8, 0xA1, 0xB2];
    serial_number.extend_from_slice(&authorization_id[..12]);
    let now = unix_ms();
    let not_after_unix_ms = now + 600_000;
    let delivery_deadline_unix_ms = now + 300_000;
    let payload = pending_payload(
        key,
        generation,
        binding_id,
        approval_id,
        delivery_id,
        &spki_sha256,
        &certificate_der,
        &certificate_sha256,
        &serial_number,
        not_after_unix_ms,
        delivery_deadline_unix_ms,
        now.saturating_sub(100),
    );

    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.device_registration_bindings \
         (registration_key_digest, binding_id, organization_id, workspace_id, device_id, \
          authorization_generation, csr_sha256, spki_sha256) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(sha256(&authorization_id).to_vec())
    .bind(binding_id)
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .bind(i64::try_from(generation).expect("generation fits"))
    .bind(csr_sha256.to_vec())
    .bind(spki_sha256.to_vec())
    .execute(pool)
    .await
    .expect("seed immutable Directory binding");

    sqlx::query(
        "INSERT INTO cyrene_workspace_device_authorization.authorizations \
         (id, device_code_hash, user_code_key_version, user_code_mac, organization_id, \
          workspace_id, csr_der, csr_sha256, spki_sha256, created_at_unix_ms, \
          expires_at_unix_ms, poll_interval_ms, revision, state_kind, approval_id, state_payload, \
          state_deadline_unix_ms, registration_binding_id, device_id, authorization_generation, \
          delivery_certificate_not_after_unix_ms) \
         VALUES ($1, $2, 1, $3, $4, $5, $6, $7, $8, $9, $10, 1000, 0, \
                 'delivery_pending', $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(authorization_id.to_vec())
    .bind(sha256(&delivery_id).to_vec())
    .bind(sha256(&approval_id).to_vec())
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&csr_der)
    .bind(csr_sha256.to_vec())
    .bind(spki_sha256.to_vec())
    .bind(i64::try_from(now).expect("timestamp fits"))
    .bind(i64::try_from(now + 3_600_000).expect("timestamp fits"))
    .bind(approval_id.to_vec())
    .bind(payload)
    .bind(i64::try_from(delivery_deadline_unix_ms).expect("timestamp fits"))
    .bind(binding_id.as_bytes().to_vec())
    .bind(&key.device_id)
    .bind(i64::try_from(generation).expect("generation fits"))
    .bind(i64::try_from(not_after_unix_ms).expect("timestamp fits"))
    .execute(pool)
    .await
    .expect("seed durable delivery authorization");

    DeliveryFixture {
        authorization_id,
        approval_id,
        binding_id,
        generation,
        delivery_id,
        csr_der,
        csr_sha256,
        spki_sha256,
        certificate_der,
        certificate_sha256,
        serial_number,
        device_id: key.device_id.clone(),
        not_after_unix_ms,
        delivery_deadline_unix_ms,
    }
}

fn pending_payload(
    key: &WorkspaceDeviceKey,
    generation: u64,
    binding_id: Uuid,
    approval_id: [u8; 16],
    delivery_id: [u8; 16],
    spki_sha256: &[u8; 32],
    certificate_der: &[u8],
    certificate_sha256: &[u8; 32],
    serial_number: &[u8],
    not_after_unix_ms: u64,
    delivery_deadline_unix_ms: u64,
    checked_at_unix_ms: u64,
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format_version": 5,
        "state": {
            "kind": "delivery_pending",
            "approval_id": approval_id,
            "certificate_validation": {
                "validation_version": 1,
                "certificate_sha256": certificate_sha256,
                "registration_binding_id": binding_id.as_bytes(),
                "checked_at_unix_ms": checked_at_unix_ms,
            },
            "certificate": {
                "certificate_der": certificate_der,
                "ca_chain_der": [],
                "serial_number": serial_number,
                "registration_binding_id": binding_id.as_bytes(),
                "device_key": {
                    "organization_id": key.organization_id,
                    "workspace_id": key.workspace_id,
                    "device_id": key.device_id,
                },
                "authorization_generation": generation,
                "scope": {
                    "organization_id": key.organization_id,
                    "workspace_id": key.workspace_id,
                },
                "spki_sha256": spki_sha256,
                "not_after_unix_ms": not_after_unix_ms,
            },
            "delivery_id": delivery_id,
            "certificate_sha256": certificate_sha256,
            "delivery_deadline_unix_ms": delivery_deadline_unix_ms,
        }
    }))
    .expect("encode delivery payload")
}

async fn mark_delivered(pool: &PgPool, fixture: &DeliveryFixture, key: &WorkspaceDeviceKey) {
    let acknowledged_at_unix_ms = unix_ms().saturating_sub(20);
    let payload = serde_json::to_vec(&json!({
        "format_version": 5,
        "state": {
            "kind": "delivered",
            "approval_id": fixture.approval_id,
            "certificate": {
                "certificate_der": fixture.certificate_der,
                "ca_chain_der": [],
                "serial_number": fixture.serial_number,
                "registration_binding_id": fixture.binding_id.as_bytes(),
                "device_key": {
                    "organization_id": key.organization_id,
                    "workspace_id": key.workspace_id,
                    "device_id": key.device_id,
                },
                "authorization_generation": fixture.generation,
                "scope": {
                    "organization_id": key.organization_id,
                    "workspace_id": key.workspace_id,
                },
                "spki_sha256": fixture.spki_sha256,
                "not_after_unix_ms": fixture.not_after_unix_ms,
            },
            "receipt": {
                "authorization_id": fixture.authorization_id,
                "delivery_id": fixture.delivery_id,
                "device_id": fixture.device_id,
                "authorization_generation": fixture.generation,
                "certificate_sha256": fixture.certificate_sha256,
                "csr_sha256": fixture.csr_sha256,
                "csr_spki_sha256": fixture.spki_sha256,
                "acknowledged_at_unix_ms": acknowledged_at_unix_ms,
            },
            "certificate_validation": {
                "validation_version": 1,
                "certificate_sha256": fixture.certificate_sha256,
                "registration_binding_id": fixture.binding_id.as_bytes(),
                "checked_at_unix_ms": unix_ms().saturating_sub(10),
            },
        }
    }))
    .expect("encode delivered payload");
    sqlx::query(
        "UPDATE cyrene_workspace_device_authorization.authorizations \
         SET state_kind = 'delivered', state_payload = $2, state_deadline_unix_ms = NULL, \
             delivery_certificate_not_after_unix_ms = NULL \
         WHERE id = $1",
    )
    .bind(fixture.authorization_id.to_vec())
    .bind(payload)
    .execute(pool)
    .await
    .expect("persist simulated durable ACK receipt");
}

async fn advance_generation(pool: &PgPool, key: &WorkspaceDeviceKey, from: u64, to: u64) {
    let updated = sqlx::query(
        "UPDATE cyrene_workspace_directory.workspace_device_identities \
         SET current_authorization_generation = $4, updated_at = clock_timestamp() \
         WHERE organization_id = $1 AND workspace_id = $2 AND device_id = $3 \
           AND current_authorization_generation = $5",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .bind(i64::try_from(to).expect("generation fits"))
    .bind(i64::try_from(from).expect("generation fits"))
    .execute(pool)
    .await
    .expect("advance Directory generation");
    assert_eq!(updated.rows_affected(), 1);
}

async fn legacy_pending_insert(
    pool: &PgPool,
    fixture: &DeliveryFixture,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO cyrene_workspace_device_registry.certificate_records \
         (authorization_id, registration_binding_id, organization_id, workspace_id, device_id, \
          authorization_generation, delivery_id, certificate_sha256, csr_sha256, spki_sha256, \
          serial_number, not_after_unix_ms, delivery_deadline_unix_ms, state) \
         SELECT id, $2, organization_id, workspace_id, device_id, authorization_generation, \
                $3, $4, csr_sha256, spki_sha256, $5, $6, $7, 'pending_ack' \
         FROM cyrene_workspace_device_authorization.authorizations WHERE id = $1",
    )
    .bind(fixture.authorization_id.to_vec())
    .bind(fixture.binding_id)
    .bind(fixture.delivery_id.to_vec())
    .bind(fixture.certificate_sha256.to_vec())
    .bind(&fixture.serial_number)
    .bind(i64::try_from(fixture.not_after_unix_ms).expect("timestamp fits"))
    .bind(i64::try_from(fixture.delivery_deadline_unix_ms).expect("timestamp fits"))
    .execute(pool)
    .await
    .map(|_| ())
}

async fn record_late_ca_issuance(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
    fixture: &DeliveryFixture,
) {
    sqlx::query(
        "INSERT INTO cyrene_workspace_device_ca.issued_certificates \
         (authorization_id, request_sha256, registration_binding_id, organization_id, \
          workspace_id, device_id, authorization_generation, csr_der, csr_sha256, spki_sha256, \
          issued_at_unix_ms, certificate_der, certificate_sha256, serial_number, \
          not_after_unix_ms) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
    )
    .bind(fixture.authorization_id.to_vec())
    .bind(sha256(&fixture.authorization_id).to_vec())
    .bind(fixture.binding_id)
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .bind(i64::try_from(fixture.generation).expect("generation fits"))
    .bind(&fixture.csr_der)
    .bind(fixture.csr_sha256.to_vec())
    .bind(fixture.spki_sha256.to_vec())
    .bind(i64::try_from(unix_ms()).expect("timestamp fits"))
    .bind(&fixture.certificate_der)
    .bind(fixture.certificate_sha256.to_vec())
    .bind(&fixture.serial_number)
    .bind(i64::try_from(fixture.not_after_unix_ms).expect("timestamp fits"))
    .execute(pool)
    .await
    .expect("seed a CA certificate committed after the revoke sweep");
}

async fn wait_for_advisory_waiters(pool: &PgPool, expected: i64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE application_name = $1 AND wait_event_type = 'Lock' \
                   AND wait_event = 'advisory'",
            )
            .bind(APPLICATION_NAME)
            .fetch_one(pool)
            .await
            .expect("inspect isolated PostgreSQL lock waiters");
            if count >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("database operations queued on the same device-key lock");
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time is after Unix epoch")
        .as_millis() as u64
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}
