//! PostgreSQL integration coverage for durable Authority workload delegation.
//!
//! This test requires a fresh, isolated database after all Workspace migrations. It is ignored
//! in ordinary test runs so ambient databases are never modified accidentally.

use cy_proto::cyrene::workspace::authority::v2 as wire;
use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_workspace_control_plane::authority::{
    AuthorityDelegation, AuthorityDelegationError, AuthorityDelegationStore, AuthorityOutboxError,
    AuthorityOutboxStore, DelegatedOperationScope, EnqueueOutcome, ExecutionDevicePeer,
    NewAuthorityInvocation,
};
use cy_workspace_control_plane::device_registry::WorkspaceDeviceKey;
use cy_workspace_postgres_storage::{
    DeviceCertificateRegistryActivation, PostgresAuthorityDelegationStore,
    PostgresWorkspaceDeviceRegistry, PostgresWorkspaceOutbox,
};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use uuid::Uuid;

const DATABASE_URL_ENV: &str = "CYRENE_WORKSPACE_AUTHORITY_DELEGATION_TEST_DATABASE_URL";
const OWNER_SOURCE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const ORGANIZATION_ID: &str = "delegation-postgres-org";
const WORKSPACE_ID: &str = "delegation-postgres-workspace";
const PRINCIPAL_ISSUER: &str = "https://issuer.example/tenant-a";
const PRINCIPAL_SUBJECT: &str = "synthetic-user-91";
const WORKLOAD_AUDIENCE: &str = "exchange";
const MANAGED_WORKLOAD_ID: &str = "exchange-production-v1";
const OPERATION_OWNER_ID: &str = "exchange";
const OPERATION_ID: &str = "jobs.read";
const RESOURCE_ID: &str = "synthetic-job-17";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an isolated PostgreSQL database with all Workspace migrations applied"]
async fn delegation_persists_scopes_and_fences_postgres_outbox_claims() {
    let database_url = std::env::var(DATABASE_URL_ENV).expect("isolated test database URL is set");
    let admin_pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .expect("connect to isolated migrated PostgreSQL");
    let delegation_table_exists: bool = sqlx::query_scalar(
        "SELECT to_regclass('cyrene_workspace_device_registry.authority_delegations') IS NOT NULL",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect migration result");
    assert!(
        delegation_table_exists,
        "migration 0012 must be applied by the gate"
    );
    let app_can_read_device_fence: bool = sqlx::query_scalar(
        "SELECT has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'organization_id', 'SELECT'\
         ) AND has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'workspace_id', 'SELECT'\
         ) AND has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'device_id', 'SELECT'\
         ) AND has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'current_authorization_generation', 'SELECT'\
         )",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect the narrow Authority device-fence grant");
    let app_can_update_device_fence: bool = sqlx::query_scalar(
        "SELECT has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'organization_id', 'UPDATE'\
         ) OR has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'workspace_id', 'UPDATE'\
         ) OR has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'device_id', 'UPDATE'\
         ) OR has_column_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_directory.workspace_device_identities', \
            'current_authorization_generation', 'UPDATE'\
         )",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect absence of direct device-fence mutation privilege");
    let app_can_execute_device_fence: bool = sqlx::query_scalar(
        "SELECT has_function_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_device_registry.authority_execution_device_current(\
                text, text, text, bytea, bytea, bigint)', 'EXECUTE'\
         )",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect the constrained Authority device-fence function");
    let app_can_update_target_binding: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_device_registry.authority_execution_target_bindings', \
            'UPDATE'\
         )",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect the read-only target-binding role");
    let app_can_lock_target_binding: bool = sqlx::query_scalar(
        "SELECT has_function_privilege(\
            'cyrene_workspace_device_registry_app', \
            'cyrene_workspace_device_registry.lock_authority_execution_target_binding(\
                text, text, text, text, text, text, bigint, bytea, bytea, text, \
                text, bigint, text)', 'EXECUTE'\
         )",
    )
    .fetch_one(&admin_pool)
    .await
    .expect("inspect the exact target-binding lock function");
    assert!(
        app_can_read_device_fence,
        "migration 0012 must preserve the narrow Directory identity read columns"
    );
    assert!(
        !app_can_update_device_fence,
        "migration 0012 must keep Directory device identity columns read-only"
    );
    assert!(
        app_can_execute_device_fence,
        "migration 0012 must expose only the constrained device-fence operation"
    );
    assert!(
        !app_can_update_target_binding,
        "registry app must not be able to mutate operator-owned target bindings"
    );
    assert!(
        app_can_lock_target_binding,
        "registry app must use the constrained target-binding lock function"
    );

    let runtime_role = format!("cyrene_delegation_test_{}", Uuid::new_v4().simple());
    let runtime_password = Uuid::new_v4().simple().to_string();
    sqlx::query(&format!(
        "CREATE ROLE {runtime_role} LOGIN PASSWORD '{runtime_password}'"
    ))
    .execute(&admin_pool)
    .await
    .expect("create synthetic runtime login");
    sqlx::query(&format!(
        "GRANT cyrene_workspace_device_registry_app TO {runtime_role}"
    ))
    .execute(&admin_pool)
    .await
    .expect("grant only the Workspace Registry app role");

    let mut runtime_url = url::Url::parse(&database_url).expect("valid isolated database URL");
    runtime_url
        .set_username(&runtime_role)
        .expect("set synthetic runtime login");
    runtime_url
        .set_password(Some(&runtime_password))
        .expect("set synthetic runtime password");
    let runtime_url = runtime_url.to_string();

    let key = WorkspaceDeviceKey {
        organization_id: ORGANIZATION_ID.to_owned(),
        workspace_id: WORKSPACE_ID.to_owned(),
        device_id: format!("delegation-device-{}", Uuid::new_v4().simple()),
    };
    let device = seed_active_device(&admin_pool, &runtime_url, &key).await;
    seed_target_binding(&admin_pool, &key, &device).await;

    let grant = AuthorityDelegation {
        delegation_id: Uuid::new_v4(),
        generation: 1,
        organization_id: ORGANIZATION_ID.to_owned(),
        workspace_id: WORKSPACE_ID.to_owned(),
        principal_issuer: PRINCIPAL_ISSUER.to_owned(),
        principal_subject: PRINCIPAL_SUBJECT.to_owned(),
        audience: WORKLOAD_AUDIENCE.to_owned(),
        managed_workload_id: MANAGED_WORKLOAD_ID.to_owned(),
        operations: vec![DelegatedOperationScope {
            owner_id: OPERATION_OWNER_ID.to_owned(),
            operation_id: OPERATION_ID.to_owned(),
            resource_ids: vec![RESOURCE_ID.to_owned()],
        }],
        expires_at_unix_ms: unix_ms() + 3_600_000,
        revoked_at_unix_ms: None,
    };
    let delegation_store = PostgresAuthorityDelegationStore::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&runtime_url)
            .await
            .expect("connect with synthetic app role"),
    );
    let created = delegation_store
        .create(grant.clone())
        .await
        .expect("persist a narrowly scoped workload grant");
    assert_eq!(created, grant);
    assert!(matches!(
        delegation_store
            .get("another-org", WORKSPACE_ID, grant.delegation_id)
            .await,
        Err(AuthorityDelegationError::NotFound)
    ));
    assert!(matches!(
        delegation_store
            .get(ORGANIZATION_ID, "another-workspace", grant.delegation_id)
            .await,
        Err(AuthorityDelegationError::NotFound)
    ));

    let persisted_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&runtime_url)
        .await
        .expect("reconnect after Authority process restart");
    drop(delegation_store);
    let delegation_store = PostgresAuthorityDelegationStore::new(persisted_pool.clone());
    assert_eq!(
        delegation_store
            .get(ORGANIZATION_ID, WORKSPACE_ID, grant.delegation_id)
            .await
            .expect("read persisted grant after reconnect"),
        grant
    );

    let outbox = PostgresWorkspaceOutbox::new(persisted_pool.clone());
    let mismatch_cases = [
        (
            "https://issuer.example/tenant-b",
            PRINCIPAL_SUBJECT,
            WORKLOAD_AUDIENCE,
        ),
        (PRINCIPAL_ISSUER, "synthetic-user-92", WORKLOAD_AUDIENCE),
        (PRINCIPAL_ISSUER, PRINCIPAL_SUBJECT, "exchange-staging"),
    ];
    for (issuer, subject, audience) in mismatch_cases {
        let invalid = delegated_invocation(&device, &key, &grant, issuer, subject, audience);
        let invocation_id = invalid.approved_invocation.invocation_id.clone();
        let enqueue_result = outbox.enqueue_authorized(invalid).await;
        assert!(
            matches!(
                &enqueue_result,
                Err(AuthorityOutboxError::AuthorizationRevoked)
            ),
            "unexpected mismatched-scope enqueue result: {enqueue_result:?}"
        );
        let persisted_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM cyrene_workspace_device_registry.workspace_invocation_outbox \
             WHERE invocation_id = $1",
        )
        .bind(invocation_id)
        .fetch_one(&persisted_pool)
        .await
        .expect("verify mismatched principal or audience did not persist");
        assert_eq!(persisted_count, 0);
    }

    let first = delegated_invocation(
        &device,
        &key,
        &grant,
        PRINCIPAL_ISSUER,
        PRINCIPAL_SUBJECT,
        WORKLOAD_AUDIENCE,
    );
    let second = delegated_invocation(
        &device,
        &key,
        &grant,
        PRINCIPAL_ISSUER,
        PRINCIPAL_SUBJECT,
        WORKLOAD_AUDIENCE,
    );
    let first_id = first.approved_invocation.invocation_id.clone();
    let second_id = second.approved_invocation.invocation_id.clone();
    assert!(matches!(
        outbox
            .enqueue_authorized(first)
            .await
            .expect("enqueue delegated approval"),
        EnqueueOutcome::Inserted(_)
    ));
    assert!(matches!(
        outbox
            .enqueue_authorized(second)
            .await
            .expect("enqueue second delegated approval"),
        EnqueueOutcome::Inserted(_)
    ));

    let authorization_columns = sqlx::query(
        "SELECT authorization_source, session_id, session_generation, delegation_id, \
                delegation_generation, delegation_audience \
         FROM cyrene_workspace_device_registry.workspace_invocation_outbox \
         WHERE invocation_id = $1",
    )
    .bind(&first_id)
    .fetch_one(&persisted_pool)
    .await
    .expect("read delegated authorization source");
    assert_eq!(
        authorization_columns
            .try_get::<String, _>("authorization_source")
            .expect("read authorization source"),
        "DELEGATION"
    );
    assert!(authorization_columns
        .try_get::<Option<Uuid>, _>("session_id")
        .expect("read optional interactive session")
        .is_none());
    assert_eq!(
        authorization_columns
            .try_get::<i64, _>("session_generation")
            .expect("read legacy session generation"),
        0
    );
    assert_eq!(
        authorization_columns
            .try_get::<Option<Uuid>, _>("delegation_id")
            .expect("read delegation identity"),
        Some(grant.delegation_id)
    );
    assert_eq!(
        authorization_columns
            .try_get::<Option<i64>, _>("delegation_generation")
            .expect("read delegation generation"),
        Some(1)
    );
    assert_eq!(
        authorization_columns
            .try_get::<Option<String>, _>("delegation_audience")
            .expect("read delegation audience"),
        Some(WORKLOAD_AUDIENCE.to_owned())
    );
    let synthetic_session_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cyrene_workspace_device_registry.authority_web_sessions",
    )
    .fetch_one(&persisted_pool)
    .await
    .expect("verify the delegated path did not create a web session");
    assert_eq!(synthetic_session_count, 0);

    let session_fk: Option<String> = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint \
         WHERE conrelid = 'cyrene_workspace_device_registry.workspace_invocation_outbox'::regclass \
           AND conname = 'workspace_invocation_outbox_session_fk' AND contype = 'f'",
    )
    .fetch_optional(&persisted_pool)
    .await
    .expect("inspect retained interactive session foreign key");
    assert!(session_fk
        .as_deref()
        .is_some_and(|definition| definition.contains("authority_web_sessions(session_id)")));
    let source_default: Option<String> = sqlx::query_scalar(
        "SELECT column_default FROM information_schema.columns \
         WHERE table_schema = 'cyrene_workspace_device_registry' \
           AND table_name = 'workspace_invocation_outbox' \
           AND column_name = 'authorization_source'",
    )
    .fetch_optional(&persisted_pool)
    .await
    .expect("inspect the legacy-row source default");
    assert!(source_default
        .as_deref()
        .is_some_and(|value| value.contains("INTERACTIVE_SESSION")));

    let peer = ExecutionDevicePeer {
        organization_id: key.organization_id.clone(),
        workspace_id: key.workspace_id.clone(),
        device_id: key.device_id.clone(),
        authorization_id: Uuid::from_bytes(device.authorization_id),
        authorization_generation: 1,
        certificate_fingerprint_sha256: device.certificate_sha256,
    };
    let claimed = outbox
        .claim_for_execution_device(&peer, 1)
        .await
        .expect("claim the valid delegated invocation");
    assert_eq!(claimed.len(), 1);
    let claimed_id = claimed[0].approved_invocation.invocation_id.clone();
    assert!(claimed_id == first_id || claimed_id == second_id);
    let pending_id = if claimed_id == first_id {
        second_id.clone()
    } else {
        first_id.clone()
    };
    let credential = claimed[0]
        .approved_invocation
        .credential
        .as_ref()
        .expect("claimed invocation has signed credential");
    assert_eq!(
        credential.authorization_source,
        wire::InvocationAuthorizationSource::Delegation as i32
    );
    assert!(credential.session_id.is_empty());
    assert_eq!(credential.session_generation, 0);
    outbox
        .validate_product_execution_authorization(credential)
        .await
        .expect("active claim passes the PostgreSQL delegation fence");

    let revoked = delegation_store
        .revoke(
            ORGANIZATION_ID,
            WORKSPACE_ID,
            grant.delegation_id,
            unix_ms(),
        )
        .await
        .expect("revoke the delegated grant");
    assert_eq!(revoked.generation, 2);
    assert!(revoked.revoked_at_unix_ms.is_some());
    let repeated_revoke = delegation_store
        .revoke(
            ORGANIZATION_ID,
            WORKSPACE_ID,
            grant.delegation_id,
            unix_ms() + 1,
        )
        .await
        .expect("repeated revocation is stable");
    assert_eq!(repeated_revoke.generation, 2);
    assert_eq!(
        repeated_revoke.revoked_at_unix_ms,
        revoked.revoked_at_unix_ms
    );
    assert!(matches!(
        outbox
            .validate_product_execution_authorization(credential)
            .await,
        Err(AuthorityOutboxError::AuthorizationRevoked)
    ));

    let retired_claim = sqlx::query(
        "UPDATE cyrene_workspace_device_registry.workspace_invocation_outbox \
         SET status = 'FAILED', error_code = 'synthetic-revocation-test' \
         WHERE invocation_id = $1",
    )
    .bind(&claimed_id)
    .execute(&admin_pool)
    .await
    .expect("retire the already-invalid active claim for the pending-row assertion");
    assert_eq!(retired_claim.rows_affected(), 1);
    assert!(outbox
        .claim_for_execution_device(&peer, 8)
        .await
        .expect("claim query excludes a revoked grant")
        .is_empty());
    let pending_generation: (String, i64, i64) = sqlx::query_as(
        "SELECT status, claim_count, session_generation \
         FROM cyrene_workspace_device_registry.workspace_invocation_outbox \
         WHERE invocation_id = $1",
    )
    .bind(&pending_id)
    .fetch_one(&persisted_pool)
    .await
    .expect("read the sibling delegated row after revocation");
    assert_eq!(pending_generation, ("PENDING".to_owned(), 0, 0));

    let immutable_scope_update = sqlx::query(
        "UPDATE cyrene_workspace_device_registry.authority_delegations \
         SET principal_issuer = 'https://issuer.example/rewritten' WHERE delegation_id = $1",
    )
    .bind(grant.delegation_id)
    .execute(&admin_pool)
    .await;
    assert!(
        immutable_scope_update.is_err(),
        "delegation scope remains immutable"
    );

    let rollback_guard = sqlx::query(
        "DO $rollback_guard$ BEGIN \
           IF EXISTS (SELECT 1 FROM cyrene_workspace_device_registry.workspace_invocation_outbox \
                      WHERE authorization_source = 'DELEGATION') THEN \
             RAISE EXCEPTION 'delegated invocation rows prevent this rollback'; \
           END IF; \
         END $rollback_guard$;",
    )
    .execute(&admin_pool)
    .await;
    assert!(
        rollback_guard.is_err(),
        "rollback guard preserves delegated rows"
    );
    let rollback_error = rollback_guard.unwrap_err().to_string();
    assert!(rollback_error.contains("delegated invocation rows prevent this rollback"));

    persisted_pool.close().await;
    admin_pool.close().await;
}

#[derive(Clone)]
struct ActiveDeviceFixture {
    authorization_id: [u8; 16],
    certificate_sha256: [u8; 32],
}

struct DeliveryFixture {
    authorization_id: [u8; 16],
    approval_id: [u8; 16],
    binding_id: Uuid,
    generation: u64,
    delivery_id: [u8; 16],
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
    certificate_der: Vec<u8>,
    certificate_sha256: [u8; 32],
    serial_number: Vec<u8>,
    device_id: String,
    not_after_unix_ms: u64,
}

/// Seeds a synthetic active device through the same durable Directory and Registry rows used by
/// the production outbox fence; no browser or interactive Authority session is created.
async fn seed_active_device(
    pool: &PgPool,
    runtime_url: &str,
    key: &WorkspaceDeviceKey,
) -> ActiveDeviceFixture {
    sqlx::query(
        "INSERT INTO cyrene_workspace_directory.workspace_device_identities \
         (organization_id, workspace_id, device_id, current_authorization_generation) \
         VALUES ($1, $2, $3, 1)",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(&key.device_id)
    .execute(pool)
    .await
    .expect("seed current synthetic Directory device identity");

    let delivery = seed_pending_delivery(pool, key).await;
    let registry = PostgresWorkspaceDeviceRegistry::connect(runtime_url)
        .expect("connect the Registry using the synthetic app role");
    registry
        .stage_pending_delivery(&delivery.authorization_id)
        .expect("stage a pending synthetic certificate");
    mark_delivery_acknowledged(pool, &delivery, key).await;
    assert_eq!(
        registry
            .activate_acknowledged_delivery(&delivery.authorization_id)
            .expect("activate the acknowledged synthetic certificate"),
        DeviceCertificateRegistryActivation::Activated
    );
    drop(registry);
    ActiveDeviceFixture {
        authorization_id: delivery.authorization_id,
        certificate_sha256: delivery.certificate_sha256,
    }
}

async fn seed_pending_delivery(pool: &PgPool, key: &WorkspaceDeviceKey) -> DeliveryFixture {
    let binding_id = Uuid::new_v4();
    let authorization_id = Uuid::new_v4().into_bytes();
    let approval_id = Uuid::new_v4().into_bytes();
    let delivery_id = Uuid::new_v4().into_bytes();
    let generation = 1;
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
    let payload = pending_payload(PendingCertificatePayload {
        key,
        generation,
        binding_id,
        approval_id,
        delivery_id,
        spki_sha256: &spki_sha256,
        certificate_der: &certificate_der,
        certificate_sha256: &certificate_sha256,
        serial_number: &serial_number,
        not_after_unix_ms,
        delivery_deadline_unix_ms,
        checked_at_unix_ms: now.saturating_sub(100),
    });

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
    .expect("seed immutable synthetic Directory registration binding");

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
    .expect("seed synthetic pending device authorization");

    DeliveryFixture {
        authorization_id,
        approval_id,
        binding_id,
        generation,
        delivery_id,
        csr_sha256,
        spki_sha256,
        certificate_der,
        certificate_sha256,
        serial_number,
        device_id: key.device_id.clone(),
        not_after_unix_ms,
    }
}

async fn mark_delivery_acknowledged(
    pool: &PgPool,
    fixture: &DeliveryFixture,
    key: &WorkspaceDeviceKey,
) {
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
    .expect("encode synthetic delivery acknowledgement");
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
    .expect("persist synthetic delivery acknowledgement");
}

struct PendingCertificatePayload<'a> {
    key: &'a WorkspaceDeviceKey,
    generation: u64,
    binding_id: Uuid,
    approval_id: [u8; 16],
    delivery_id: [u8; 16],
    spki_sha256: &'a [u8; 32],
    certificate_der: &'a [u8],
    certificate_sha256: &'a [u8; 32],
    serial_number: &'a [u8],
    not_after_unix_ms: u64,
    delivery_deadline_unix_ms: u64,
    checked_at_unix_ms: u64,
}

fn pending_payload(input: PendingCertificatePayload<'_>) -> Vec<u8> {
    let PendingCertificatePayload {
        key,
        generation,
        binding_id,
        approval_id,
        delivery_id,
        spki_sha256,
        certificate_der,
        certificate_sha256,
        serial_number,
        not_after_unix_ms,
        delivery_deadline_unix_ms,
        checked_at_unix_ms,
    } = input;
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
    .expect("encode synthetic pending certificate payload")
}

async fn seed_target_binding(
    pool: &PgPool,
    key: &WorkspaceDeviceKey,
    device: &ActiveDeviceFixture,
) {
    let target_manifest = [0x61_u8; 32];
    let bundle_manifest = [0x62_u8; 32];
    let configured_at = unix_ms();
    sqlx::query(
        "INSERT INTO cyrene_workspace_device_registry.authority_execution_target_bindings \
         (organization_id, workspace_id, operation_owner_id, operation_id, target_component, \
          execution_device_id, execution_device_generation, execution_authorization_id, \
          certificate_fingerprint_sha256, target_binding_manifest_sha256, bundle_manifest_sha256, \
          contract_activation_generation, owner_source_commit, endpoint_base, active, \
          configured_at_unix_ms, configured_by, configuration_reason) \
         VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8, $9, $10, 4, $11, $12, TRUE, $13, $14, $15)",
    )
    .bind(&key.organization_id)
    .bind(&key.workspace_id)
    .bind(OPERATION_OWNER_ID)
    .bind(OPERATION_ID)
    .bind("cy-workspace-exchange")
    .bind(&key.device_id)
    .bind(device.authorization_id.to_vec())
    .bind(device.certificate_sha256.to_vec())
    .bind(target_manifest.to_vec())
    .bind(bundle_manifest.to_vec())
    .bind(OWNER_SOURCE_COMMIT)
    .bind("https://exchange.example.invalid/v1/jobs/")
    .bind(i64::try_from(configured_at).expect("timestamp fits"))
    .bind("synthetic-test-fixture")
    .bind("isolated PostgreSQL integration test")
    .execute(pool)
    .await
    .expect("seed operator-owned synthetic target binding");
    assert_ne!(target_manifest, [0; 32]);
    assert_ne!(bundle_manifest, [0; 32]);
}

fn delegated_invocation(
    device: &ActiveDeviceFixture,
    key: &WorkspaceDeviceKey,
    grant: &AuthorityDelegation,
    issuer: &str,
    subject: &str,
    audience: &str,
) -> NewAuthorityInvocation {
    let invocation_id = Uuid::new_v4().to_string();
    let idempotency_key = format!("delegated-{}", Uuid::new_v4().simple());
    let endpoint = "https://exchange.example.invalid/v1/jobs/synthetic-job-17".to_owned();
    let issued_at_unix_ms = unix_ms();
    let expires_at_unix_ms = issued_at_unix_ms + 3_600_000;
    let issued_at = timestamp(issued_at_unix_ms);
    let expires_at = timestamp(expires_at_unix_ms);
    let target_manifest = [0x61; 32];
    let bundle_manifest = [0x62; 32];
    let invocation = ProductApiInvocationV2 {
        owner_id: OPERATION_OWNER_ID.to_owned(),
        operation_id: OPERATION_ID.to_owned(),
        json_body: br#"{"query":"synthetic"}"#.to_vec(),
        resource_id: RESOURCE_ID.to_owned(),
        idempotency_key: idempotency_key.clone(),
    };
    let target = wire::ExecutionTarget {
        target_component: "cy-workspace-exchange".to_owned(),
        http_method: "GET".to_owned(),
        endpoint: endpoint.clone(),
        resource_id: RESOURCE_ID.to_owned(),
        scope: "jobs.read".to_owned(),
        idempotency_key: idempotency_key.clone(),
        idempotency_semantics: wire::IdempotencySemantics::Required as i32,
        execution_device_id: key.device_id.clone(),
        execution_device_generation: 1,
        execution_authorization_id: device.authorization_id.to_vec(),
        execution_device_certificate_sha256: device.certificate_sha256.to_vec(),
    };
    let envelope = wire::CanonicalInvocationEnvelope {
        invocation_id: invocation_id.clone(),
        invocation: Some(invocation),
        target: Some(target),
        organization_id: key.organization_id.clone(),
        workspace_id: key.workspace_id.clone(),
        principal_issuer: issuer.to_owned(),
        principal_subject: subject.to_owned(),
        execution_device_id: key.device_id.clone(),
        execution_device_generation: 1,
        execution_authorization_id: device.authorization_id.to_vec(),
        execution_device_certificate_sha256: device.certificate_sha256.to_vec(),
        session_id: String::new(),
        session_generation: 0,
        contract_activation_generation: 4,
        issued_at: Some(issued_at),
        expires_at: Some(expires_at),
        authorization_source: wire::InvocationAuthorizationSource::Delegation as i32,
        delegation_id: grant.delegation_id.to_string(),
        delegation_generation: grant.generation,
        delegation_audience: audience.to_owned(),
    };
    let invocation_digest = Sha256::digest(
        envelope
            .invocation
            .as_ref()
            .expect("invocation exists")
            .encode_to_vec(),
    )
    .to_vec();
    let request_digest = Sha256::digest(envelope.encode_to_vec()).to_vec();
    let credential = wire::ExecutionCredential {
        invocation_id: invocation_id.clone(),
        invocation_digest_sha256: invocation_digest,
        request_digest_sha256: request_digest,
        organization_id: key.organization_id.clone(),
        workspace_id: key.workspace_id.clone(),
        principal_issuer: issuer.to_owned(),
        principal_subject: subject.to_owned(),
        operation_owner_id: OPERATION_OWNER_ID.to_owned(),
        operation_id: OPERATION_ID.to_owned(),
        scope: "jobs.read".to_owned(),
        resource_id: RESOURCE_ID.to_owned(),
        target_component: "cy-workspace-exchange".to_owned(),
        http_method: "GET".to_owned(),
        endpoint,
        execution_device_id: key.device_id.clone(),
        execution_device_generation: 1,
        execution_authorization_id: device.authorization_id.to_vec(),
        execution_device_certificate_sha256: device.certificate_sha256.to_vec(),
        session_id: String::new(),
        session_generation: 0,
        contract_activation_generation: 4,
        idempotency_key,
        idempotency_semantics: wire::IdempotencySemantics::Required as i32,
        signing_key_id: "synthetic-delegation-test-key".to_owned(),
        issued_at: Some(issued_at),
        expires_at: Some(expires_at),
        authority_signature: vec![0x7f],
        authorization_source: wire::InvocationAuthorizationSource::Delegation as i32,
        delegation_id: grant.delegation_id.to_string(),
        delegation_generation: grant.generation,
        delegation_audience: audience.to_owned(),
    };
    let canonical_envelope_proto = envelope.encode_to_vec();
    NewAuthorityInvocation {
        approved_invocation: wire::ApprovedInvocation {
            invocation_id,
            canonical_envelope: Some(envelope),
            credential: Some(credential),
        },
        canonical_envelope_proto,
        target_binding_manifest_sha256: target_manifest,
        bundle_manifest_sha256: bundle_manifest,
        owner_source_commit: OWNER_SOURCE_COMMIT.to_owned(),
    }
}

fn timestamp(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: (unix_ms / 1_000) as i64,
        nanos: ((unix_ms % 1_000) * 1_000_000) as i32,
    }
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
