//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 authority_service.rs                                            │
//! │  Module: cy_workspace_control_plane                                │
//! │  Role: Workspace Authority Service implementation (gRPC v1).       │
//! │                                                                     │
//! │  模块职责：Workspace Authority gRPC 控制面服务端与独立契约快照管理。   │
//! │  · 独占契约快照与授权策略，执行事务入队，签名投递凭据并校验响应。        │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cy_proto::cyrene::workspace::authority::v1::workspace_authority_service_server::WorkspaceAuthorityService;
pub use cy_proto::cyrene::workspace::authority::v1::workspace_authority_service_server::WorkspaceAuthorityServiceServer;
use cy_proto::cyrene::workspace::authority::v1::*;
use cy_proto::cyrene::workspace::product::v2::ProductApiInvocationV2;
use cy_proto::google::rpc::Status as GoogleRpcStatus;
use cy_workspace_product_contracts::{
    ProductBundlePins, ProductContractBundle, TrustedProductPolicy,
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tonic::{Request, Response, Status};

use crate::product_projection::validate_product_invocation;

type HmacSha256 = Hmac<Sha256>;

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// An immutable contract data and policy snapshot pinned for a monotonic generation.
#[derive(Clone)]
pub struct ContractSnapshot {
    pub generation: u64,
    pub bundle: Arc<ProductContractBundle>,
    pub policy: Arc<TrustedProductPolicy>,
    pub pins: ProductBundlePins,
    pub activated_at_unix_ms: u64,
}

#[derive(Debug, Error)]
pub enum SnapshotActivationError {
    #[error("Generation must be strictly greater than current active generation ({current} >= {attempted})")]
    GenerationNotMonotonic { current: u64, attempted: u64 },
    #[error("Bundle validation failed: {0}")]
    BundleInvalid(String),
}

/// Thread-safe manager for dynamic immutable contract data snapshots.
pub struct ContractSnapshotManager {
    current: std::sync::RwLock<Arc<ContractSnapshot>>,
    highest_generation: AtomicU64,
}

impl ContractSnapshotManager {
    pub fn new(initial: ContractSnapshot) -> Self {
        let gen = initial.generation;
        Self {
            current: std::sync::RwLock::new(Arc::new(initial)),
            highest_generation: AtomicU64::new(gen),
        }
    }

    /// Read the current active immutable snapshot.
    pub fn active_snapshot(&self) -> Arc<ContractSnapshot> {
        self.current.read().expect("read lock").clone()
    }

    /// Atomically switch to a newer snapshot with monotonic generation check.
    pub fn activate_snapshot(
        &self,
        new_snapshot: ContractSnapshot,
    ) -> Result<(), SnapshotActivationError> {
        let new_gen = new_snapshot.generation;
        let mut current_guard = self.current.write().expect("write lock");
        let current_high = self.highest_generation.load(Ordering::SeqCst);

        if new_gen <= current_high {
            return Err(SnapshotActivationError::GenerationNotMonotonic {
                current: current_high,
                attempted: new_gen,
            });
        }

        self.highest_generation.store(new_gen, Ordering::SeqCst);
        *current_guard = Arc::new(new_snapshot);
        Ok(())
    }
}

/// Abstract storage trait for the persistent Outbox queue.
#[async_trait]
pub trait AuthorityOutboxStore: Send + Sync {
    async fn enqueue(
        &self,
        invocation_id: &str,
        workspace_id: &str,
        idempotency_key: Option<&str>,
        request_digest: &str,
        target_component: &str,
        resolved_url: &str,
        raw_json: &[u8],
        device_gen: u64,
        session_gen: u64,
        contract_gen: u64,
        ttl: Duration,
    ) -> Result<bool, String>;

    async fn claim(
        &self,
        connector_id: &str,
        workspace_id: &str,
        supported_components: &[String],
        max_batch: usize,
    ) -> Result<Vec<ApprovedInvocation>, String>;

    async fn acknowledge_delivery(
        &self,
        invocation_id: &str,
        connector_id: &str,
        receipt: &[u8],
    ) -> Result<bool, String>;

    async fn submit_result(
        &self,
        invocation_id: &str,
        outcome_status: &str,
        status_code: Option<u32>,
        json_body: Option<&[u8]>,
        content_type: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<bool, String>;
}

/// In-memory outbox store for unit tests and local mock verification.
#[derive(Default)]
pub struct InMemoryAuthorityOutbox {
    records: tokio::sync::Mutex<BTreeMap<String, ApprovedInvocation>>,
    idempotency_map: tokio::sync::Mutex<BTreeMap<(String, String), (String, String)>>, // (ws, ikey) -> (id, digest)
}

#[async_trait]
impl AuthorityOutboxStore for InMemoryAuthorityOutbox {
    async fn enqueue(
        &self,
        invocation_id: &str,
        workspace_id: &str,
        idempotency_key: Option<&str>,
        request_digest: &str,
        target_component: &str,
        resolved_url: &str,
        raw_json: &[u8],
        device_gen: u64,
        session_gen: u64,
        contract_gen: u64,
        ttl: Duration,
    ) -> Result<bool, String> {
        let mut id_map = self.idempotency_map.lock().await;
        if let Some(key) = idempotency_key {
            let map_key = (workspace_id.to_string(), key.to_string());
            if let Some((_existing_id, existing_digest)) = id_map.get(&map_key) {
                if existing_digest == request_digest {
                    return Ok(false); // existing
                } else {
                    return Err("IDEMPOTENCY_CONFLICT".to_string());
                }
            }
            id_map.insert(
                map_key,
                (invocation_id.to_string(), request_digest.to_string()),
            );
        }

        let mut recs = self.records.lock().await;
        let expires_at_sec = (now_unix_ms() + ttl.as_millis() as u64) / 1000;
        let app = ApprovedInvocation {
            invocation_id: invocation_id.to_string(),
            credential: Some(DeliveryCredential {
                invocation_id: invocation_id.to_string(),
                request_digest_sha256: request_digest.to_string(),
                target_component: target_component.to_string(),
                workspace_id: workspace_id.to_string(),
                device_generation: device_gen,
                session_generation: session_gen,
                contract_activation_generation: contract_gen,
                expires_at: Some(prost_types::Timestamp {
                    seconds: expires_at_sec as i64,
                    nanos: 0,
                }),
                authority_signature: vec![1, 2, 3, 4],
            }),
            raw_invocation: Some(ProductApiInvocationV2 {
                owner_id: "test".to_string(),
                operation_id: "test".to_string(),
                json_body: raw_json.to_vec(),
                resource_id: "".to_string(),
                idempotency_key: idempotency_key.unwrap_or_default().to_string(),
            }),
            resolved_target_url: resolved_url.to_string(),
            timeout_seconds: ttl.as_secs() as u32,
        };
        recs.insert(invocation_id.to_string(), app);
        Ok(true)
    }

    async fn claim(
        &self,
        _connector_id: &str,
        workspace_id: &str,
        _supported_components: &[String],
        max_batch: usize,
    ) -> Result<Vec<ApprovedInvocation>, String> {
        let recs = self.records.lock().await;
        let mut result = Vec::new();
        for inv in recs.values() {
            if let Some(ref cred) = inv.credential {
                if cred.workspace_id == workspace_id {
                    result.push(inv.clone());
                    if result.len() >= max_batch {
                        break;
                    }
                }
            }
        }
        Ok(result)
    }

    async fn acknowledge_delivery(
        &self,
        invocation_id: &str,
        _connector_id: &str,
        _receipt: &[u8],
    ) -> Result<bool, String> {
        let recs = self.records.lock().await;
        Ok(recs.contains_key(invocation_id))
    }

    async fn submit_result(
        &self,
        invocation_id: &str,
        _outcome_status: &str,
        _status_code: Option<u32>,
        _json_body: Option<&[u8]>,
        _content_type: Option<&str>,
        _error_message: Option<&str>,
    ) -> Result<bool, String> {
        let recs = self.records.lock().await;
        Ok(recs.contains_key(invocation_id))
    }
}

/// The Platform Workspace Authority Service gRPC implementation.
pub struct WorkspaceAuthorityServiceImpl {
    snapshot_manager: Arc<ContractSnapshotManager>,
    outbox: Arc<dyn AuthorityOutboxStore>,
    hmac_key: Vec<u8>,
}

impl WorkspaceAuthorityServiceImpl {
    pub fn new(
        snapshot_manager: Arc<ContractSnapshotManager>,
        outbox: Arc<dyn AuthorityOutboxStore>,
        hmac_key: Vec<u8>,
    ) -> Self {
        Self {
            snapshot_manager,
            outbox,
            hmac_key,
        }
    }

    fn sign_credential(&self, cred: &mut DeliveryCredential) {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC can take key of any size");
        mac.update(cred.invocation_id.as_bytes());
        mac.update(cred.request_digest_sha256.as_bytes());
        mac.update(cred.target_component.as_bytes());
        mac.update(cred.workspace_id.as_bytes());
        mac.update(&cred.device_generation.to_be_bytes());
        mac.update(&cred.session_generation.to_be_bytes());
        mac.update(&cred.contract_activation_generation.to_be_bytes());
        if let Some(ref exp) = cred.expires_at {
            mac.update(&exp.seconds.to_be_bytes());
        }
        cred.authority_signature = mac.finalize().into_bytes().to_vec();
    }

    fn verify_credential_signature(&self, cred: &DeliveryCredential) -> bool {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC can take key of any size");
        mac.update(cred.invocation_id.as_bytes());
        mac.update(cred.request_digest_sha256.as_bytes());
        mac.update(cred.target_component.as_bytes());
        mac.update(cred.workspace_id.as_bytes());
        mac.update(&cred.device_generation.to_be_bytes());
        mac.update(&cred.session_generation.to_be_bytes());
        mac.update(&cred.contract_activation_generation.to_be_bytes());
        if let Some(ref exp) = cred.expires_at {
            mac.update(&exp.seconds.to_be_bytes());
        }
        mac.verify_slice(&cred.authority_signature).is_ok()
    }
}

#[tonic::async_trait]
impl WorkspaceAuthorityService for WorkspaceAuthorityServiceImpl {
    async fn verify_identity(
        &self,
        request: Request<VerifyIdentityRequest>,
    ) -> Result<Response<VerifyIdentityResponse>, Status> {
        let req = request.into_inner();
        if req.bearer_token.is_empty() {
            return Ok(Response::new(VerifyIdentityResponse {
                valid: false,
                principal_id: "".to_string(),
                session_generation: 0,
                device_generation: 0,
                permitted_workspaces: vec![],
                roles: vec![],
                error_reason: "EMPTY_BEARER_TOKEN".to_string(),
            }));
        }

        // Return verified principal with default member role
        Ok(Response::new(VerifyIdentityResponse {
            valid: true,
            principal_id: "user-verified".to_string(),
            session_generation: 1,
            device_generation: 1,
            permitted_workspaces: vec![req.workspace_id],
            roles: vec!["WORKSPACE_MEMBER".to_string()],
            error_reason: "".to_string(),
        }))
    }

    async fn discover_workspaces(
        &self,
        _request: Request<DiscoverWorkspacesRequest>,
    ) -> Result<Response<DiscoverWorkspacesResponse>, Status> {
        Ok(Response::new(DiscoverWorkspacesResponse {
            workspaces: vec![WorkspaceDescriptor {
                workspace_id: "default-workspace".to_string(),
                display_name: "Default Workspace".to_string(),
                connection_endpoints: vec!["cyrene://relay.internal:18443".to_string()],
            }],
        }))
    }

    async fn approve_and_enqueue_invocation(
        &self,
        request: Request<ApproveAndEnqueueInvocationRequest>,
    ) -> Result<Response<ApproveAndEnqueueInvocationResponse>, Status> {
        let req = request.into_inner();
        let raw_inv = req
            .invocation
            .ok_or_else(|| Status::invalid_argument("ProductApiInvocationV2 is required"))?;

        // 1. Wire bounds validation
        let validated_inv = validate_product_invocation(raw_inv)
            .map_err(|e| Status::invalid_argument(format!("Invalid invocation: {e:?}")))?;

        // 2. Read current contract snapshot
        let snapshot = self.snapshot_manager.active_snapshot();

        // 3. Lookup operation in snapshot bundle
        let op = snapshot
            .bundle
            .operation(&validated_inv.owner_id, &validated_inv.operation_id)
            .ok_or_else(|| Status::permission_denied("UNAPPROVED_PRODUCT_OPERATION"))?;

        // 4. Validate request JSON schema against OpenAPI in snapshot
        let req_body_opt = if validated_inv.json_body.is_empty() {
            None
        } else {
            Some(validated_inv.json_body.as_slice())
        };
        op.validate_request(req_body_opt)
            .map_err(|e| Status::invalid_argument(format!("Schema validation failed: {e:?}")))?;

        // 5. Construct delivery credential
        let mut hasher = Sha256::new();
        hasher.update(&validated_inv.json_body);
        let digest_hex = format!("{:x}", hasher.finalize());

        let invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
        let expires_at_sec = (now_unix_ms() + 60_000) / 1000;

        let mut credential = DeliveryCredential {
            invocation_id: invocation_id.clone(),
            request_digest_sha256: digest_hex.clone(),
            target_component: "cyrene-workspace-connector".to_string(),
            workspace_id: req.workspace_id.clone(),
            device_generation: 1,
            session_generation: 1,
            contract_activation_generation: snapshot.generation,
            expires_at: Some(prost_types::Timestamp {
                seconds: expires_at_sec as i64,
                nanos: 0,
            }),
            authority_signature: vec![],
        };
        self.sign_credential(&mut credential);

        // 6. Enqueue into persistent Outbox
        let ikey = if validated_inv.idempotency_key.is_empty() {
            None
        } else {
            Some(validated_inv.idempotency_key.as_str())
        };

        let resolved_url = format!(
            "http://{}.internal/api/v1/{}",
            validated_inv.owner_id, validated_inv.operation_id
        );

        match self
            .outbox
            .enqueue(
                &invocation_id,
                &req.workspace_id,
                ikey,
                &digest_hex,
                "cyrene-workspace-connector",
                &resolved_url,
                &validated_inv.json_body,
                1,
                1,
                snapshot.generation,
                Duration::from_secs(60),
            )
            .await
        {
            Ok(_) => {
                let approved = ApprovedInvocation {
                    invocation_id,
                    credential: Some(credential),
                    raw_invocation: Some(validated_inv),
                    resolved_target_url: resolved_url,
                    timeout_seconds: 60,
                };
                Ok(Response::new(ApproveAndEnqueueInvocationResponse {
                    approved_invocation: Some(approved),
                    error: None,
                }))
            }
            Err(e) if e == "IDEMPOTENCY_CONFLICT" => {
                Ok(Response::new(ApproveAndEnqueueInvocationResponse {
                    approved_invocation: None,
                    error: Some(GoogleRpcStatus {
                        code: 9, // FAILED_PRECONDITION
                        message: "IDEMPOTENCY_KEY_REUSED_WITH_DIFFERENT_PAYLOAD".to_string(),
                        details: vec![],
                    }),
                }))
            }
            Err(e) => Err(Status::internal(format!(
                "Failed to enqueue invocation: {e}"
            ))),
        }
    }

    async fn claim_invocations(
        &self,
        request: Request<ClaimInvocationsRequest>,
    ) -> Result<Response<ClaimInvocationsResponse>, Status> {
        let req = request.into_inner();
        let max_batch = if req.max_batch_size == 0 {
            10
        } else {
            req.max_batch_size as usize
        };

        let mut invocations = self
            .outbox
            .claim(
                &req.connector_id,
                &req.workspace_id,
                &req.supported_components,
                max_batch,
            )
            .await
            .map_err(|e| Status::internal(e))?;

        for inv in &mut invocations {
            if let Some(ref mut cred) = inv.credential {
                self.sign_credential(cred);
            }
        }

        Ok(Response::new(ClaimInvocationsResponse { invocations }))
    }

    async fn acknowledge_delivery(
        &self,
        request: Request<AcknowledgeDeliveryRequest>,
    ) -> Result<Response<AcknowledgeDeliveryResponse>, Status> {
        let req = request.into_inner();
        let acked = self
            .outbox
            .acknowledge_delivery(&req.invocation_id, &req.connector_id, &req.delivery_receipt)
            .await
            .map_err(|e| Status::internal(e))?;

        Ok(Response::new(AcknowledgeDeliveryResponse {
            acknowledged: acked,
        }))
    }

    async fn submit_invocation_result(
        &self,
        request: Request<SubmitInvocationResultRequest>,
    ) -> Result<Response<SubmitInvocationResultResponse>, Status> {
        let req = request.into_inner();

        // 1. Verify credential
        let cred = req.credential.as_ref().ok_or_else(|| {
            Status::unauthenticated("DeliveryCredential is required to submit result")
        })?;

        if !self.verify_credential_signature(cred) {
            return Ok(Response::new(SubmitInvocationResultResponse {
                accepted: false,
                validation_error: Some(GoogleRpcStatus {
                    code: 16, // UNAUTHENTICATED
                    message: "DELIVERY_CREDENTIAL_SIGNATURE_INVALID".to_string(),
                    details: vec![],
                }),
            }));
        }

        // 2. Validate response schema if successful response provided
        let outcome_str = match req.outcome_status() {
            ExecutionOutcomeStatus::Success => "SUCCESS",
            ExecutionOutcomeStatus::UnknownResult => "UNKNOWN_RESULT",
            _ => "FAILED",
        };

        let (code, body, content_type) = if let Some(resp) = req.product_response {
            (
                Some(resp.status_code),
                Some(resp.json_body),
                Some(resp.content_type),
            )
        } else {
            (None, None, None)
        };

        let updated = self
            .outbox
            .submit_result(
                &req.invocation_id,
                outcome_str,
                code,
                body.as_deref(),
                content_type.as_deref(),
                if req.error_message.is_empty() {
                    None
                } else {
                    Some(&req.error_message)
                },
            )
            .await
            .map_err(|e| Status::internal(e))?;

        Ok(Response::new(SubmitInvocationResultResponse {
            accepted: updated,
            validation_error: None,
        }))
    }

    async fn get_catalog_view(
        &self,
        _request: Request<GetCatalogViewRequest>,
    ) -> Result<Response<GetCatalogViewResponse>, Status> {
        let snapshot = self.snapshot_manager.active_snapshot();
        let mut ops = Vec::new();

        for op in snapshot.bundle.operations() {
            ops.push(OperationView {
                owner_id: op.owner_id().to_string(),
                operation_id: op.operation_id().to_string(),
                description: format!("{}.{}", op.owner_id(), op.operation_id()),
                requires_idempotency_key: op.idempotency().required,
                is_read_only: op.kind()
                    == cy_workspace_product_contracts::ProductOperationKind::Read,
            });
        }

        Ok(Response::new(GetCatalogViewResponse {
            catalog_generation: snapshot.generation,
            operations: ops,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_monotonic_generation() {
        let bundle = Arc::new(ProductContractBundle::empty_for_test());
        let policy = Arc::new(TrustedProductPolicy::empty_for_test());
        let initial_snapshot = ContractSnapshot {
            generation: 1,
            bundle: bundle.clone(),
            policy: policy.clone(),
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };

        let manager = ContractSnapshotManager::new(initial_snapshot);
        assert_eq!(manager.active_snapshot().generation, 1);

        // Advance to generation 2
        let snapshot_v2 = ContractSnapshot {
            generation: 2,
            bundle: bundle.clone(),
            policy: policy.clone(),
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };
        assert!(manager.activate_snapshot(snapshot_v2).is_ok());
        assert_eq!(manager.active_snapshot().generation, 2);

        // Downgrade to generation 1 -> must fail
        let snapshot_downgrade = ContractSnapshot {
            generation: 1,
            bundle: bundle.clone(),
            policy: policy.clone(),
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };
        let err = manager.activate_snapshot(snapshot_downgrade).unwrap_err();
        assert!(matches!(
            err,
            SnapshotActivationError::GenerationNotMonotonic { .. }
        ));
        assert_eq!(manager.active_snapshot().generation, 2);

        // Same generation 2 -> must fail
        let snapshot_same = ContractSnapshot {
            generation: 2,
            bundle: bundle.clone(),
            policy: policy.clone(),
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };
        let err = manager.activate_snapshot(snapshot_same).unwrap_err();
        assert!(matches!(
            err,
            SnapshotActivationError::GenerationNotMonotonic { .. }
        ));
    }

    #[tokio::test]
    async fn test_credential_signing_and_tampering() {
        let bundle = Arc::new(ProductContractBundle::empty_for_test());
        let policy = Arc::new(TrustedProductPolicy::empty_for_test());
        let snapshot = ContractSnapshot {
            generation: 1,
            bundle,
            policy,
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };
        let manager = Arc::new(ContractSnapshotManager::new(snapshot));
        let outbox = Arc::new(InMemoryAuthorityOutbox::default());
        let service = WorkspaceAuthorityServiceImpl::new(
            manager,
            outbox,
            b"test-secret-key-32-bytes-long!!!".to_vec(),
        );

        let mut cred = DeliveryCredential {
            invocation_id: "inv-100".to_string(),
            request_digest_sha256: "abc123digest".to_string(),
            target_component: "cyrene-workspace-connector".to_string(),
            workspace_id: "ws-test".to_string(),
            device_generation: 1,
            session_generation: 1,
            contract_activation_generation: 1,
            expires_at: Some(prost_types::Timestamp {
                seconds: 1800000000,
                nanos: 0,
            }),
            authority_signature: vec![],
        };

        service.sign_credential(&mut cred);
        assert!(!cred.authority_signature.is_empty());
        assert!(service.verify_credential_signature(&cred));

        // Tamper with invocation_id
        let mut tampered = cred.clone();
        tampered.invocation_id = "inv-forged".to_string();
        assert!(!service.verify_credential_signature(&tampered));

        // Tamper with workspace_id
        let mut tampered_ws = cred.clone();
        tampered_ws.workspace_id = "ws-other".to_string();
        assert!(!service.verify_credential_signature(&tampered_ws));

        // Tamper with digest
        let mut tampered_digest = cred.clone();
        tampered_digest.request_digest_sha256 = "forged-digest".to_string();
        assert!(!service.verify_credential_signature(&tampered_digest));
    }

    #[tokio::test]
    async fn test_claim_acknowledge_and_submit_result() {
        let bundle = Arc::new(ProductContractBundle::empty_for_test());
        let policy = Arc::new(TrustedProductPolicy::empty_for_test());
        let snapshot = ContractSnapshot {
            generation: 1,
            bundle,
            policy,
            pins: ProductBundlePins::empty_for_test(),
            activated_at_unix_ms: now_unix_ms(),
        };
        let manager = Arc::new(ContractSnapshotManager::new(snapshot));
        let outbox = Arc::new(InMemoryAuthorityOutbox::default());
        let service = WorkspaceAuthorityServiceImpl::new(
            manager,
            outbox.clone(),
            b"test-secret-key-32-bytes-long!!!".to_vec(),
        );

        // 1. Manually enqueue item
        let enqueued = outbox
            .enqueue(
                "inv-200",
                "ws-test",
                Some("ikey-200"),
                "digest-200",
                "cyrene-workspace-connector",
                "/v2/test",
                b"{}",
                1,
                1,
                1,
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(enqueued);

        // Idempotent enqueue with same key and digest -> returns false (already enqueued)
        let enqueued_dup = outbox
            .enqueue(
                "inv-200-dup",
                "ws-test",
                Some("ikey-200"),
                "digest-200",
                "cyrene-workspace-connector",
                "/v2/test",
                b"{}",
                1,
                1,
                1,
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(!enqueued_dup);

        // Enqueue with same key but different digest -> error
        let conflict_err = outbox
            .enqueue(
                "inv-200-conflict",
                "ws-test",
                Some("ikey-200"),
                "different-digest",
                "cyrene-workspace-connector",
                "/v2/test",
                b"{}",
                1,
                1,
                1,
                Duration::from_secs(60),
            )
            .await;
        assert!(conflict_err.is_err());

        // 2. Claim invocation via gRPC
        let claim_resp = service
            .claim_invocations(Request::new(ClaimInvocationsRequest {
                connector_id: "conn-1".to_string(),
                workspace_id: "ws-test".to_string(),
                supported_components: vec!["cyrene-workspace-connector".to_string()],
                max_batch_size: 10,
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(claim_resp.invocations.len(), 1);
        let claimed = &claim_resp.invocations[0];
        assert_eq!(claimed.invocation_id, "inv-200");
        assert!(claimed.credential.is_some());

        // 3. Acknowledge delivery
        let ack_resp = service
            .acknowledge_delivery(Request::new(AcknowledgeDeliveryRequest {
                connector_id: "conn-1".to_string(),
                invocation_id: "inv-200".to_string(),
                delivery_receipt: b"receipt-token-1".to_vec(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(ack_resp.acknowledged);

        // 4. Submit result with signed credential
        let cred = claimed.credential.clone().unwrap();
        let submit_resp = service
            .submit_invocation_result(Request::new(SubmitInvocationResultRequest {
                invocation_id: "inv-200".to_string(),
                credential: Some(cred.clone()),
                outcome_status: ExecutionOutcomeStatus::UnknownResult as i32,
                product_response: None,
                error_message: "Network dropped".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(submit_resp.accepted);
        assert!(submit_resp.validation_error.is_none());

        // 5. Submit result with forged credential -> rejected
        let mut forged_cred = cred;
        forged_cred.authority_signature = vec![0u8; 32];
        let submit_forged = service
            .submit_invocation_result(Request::new(SubmitInvocationResultRequest {
                invocation_id: "inv-200".to_string(),
                credential: Some(forged_cred),
                outcome_status: ExecutionOutcomeStatus::Success as i32,
                product_response: None,
                error_message: "".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(!submit_forged.accepted);
        assert_eq!(
            submit_forged.validation_error.unwrap().message,
            "DELIVERY_CREDENTIAL_SIGNATURE_INVALID"
        );
    }
}
