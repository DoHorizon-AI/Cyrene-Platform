//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 server.rs                                                       │
//! │  Module: cy_execution_control::server                               │
//! │  Role: Authenticated Host and Runtime NodeControl sessions.         │
//! │                                                                     │
//! │  模块职责：承载已认证 Host 与 Runtime NodeControl session。           │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cy_execution_fabric::{
    semantic_lease_from_proto, validate_hello, AdmissionDisposition, EnrollmentGrant,
    EnrollmentProvider, ObservationCursor, RuntimeScope,
};
use cy_kernel_api::AuthorityCallContext;
use cy_kernel_contract as semantic;
use cy_proto::core_v1::{
    self, control_plane_to_node, kernel_authority_command, kernel_authority_command_result,
    kernel_command, kernel_command_result, lease_renewal_result,
    node_control_service_server::NodeControlService, node_to_control_plane, AssignmentAck,
    AssignmentAckDisposition, ExecutionAgentHello, ExecutionAgentWelcome, KernelAuthorityCommand,
    KernelCommand, KernelCommandResult, LeaseRenewalRequest, LeaseRenewalResult, NodeHello,
    NodeToControlPlane,
};
use cy_proto::semantic_v1;
use prost::Message;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Code, Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::authentication::{AuthenticatedAgent, PeerAuthenticator};
use crate::session::{HostSession, NodeKey, OutboundItem, RuntimeSession, SessionHandle};
use crate::DispatchError;

type ResponseStream = Pin<Box<dyn Stream<Item = OutboundItem> + Send + 'static>>;

const MAX_WORKLOAD_ACTIONS: usize = 64;
const MAX_WORKLOAD_USER_BYTES: usize = 256;
const MAX_WORKLOAD_ACTION_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub struct ControlObservation {
    pub agent: AuthenticatedAgent,
    pub session_id: String,
    pub frame: NodeToControlPlane,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeSessionView {
    pub node: core_v1::NodeRef,
    pub organization_id: String,
    pub workspace_id: String,
    pub workload_identity: semantic::Identity,
    pub workload_identity_expires_at_unix_ms: u64,
}

pub(crate) struct LeaseAcquisition<'a> {
    pub node: &'a core_v1::NodeRef,
    pub command_id: &'a str,
    pub context: &'a AuthorityCallContext,
    pub holder: &'a semantic::Identity,
    pub query: &'a semantic::ResourceQuery,
    pub ttl: Duration,
    pub response_timeout: Duration,
}

#[derive(Default)]
struct Registry {
    hosts: BTreeMap<NodeKey, HostSession>,
    runtimes: BTreeMap<RuntimeKey, RuntimeSession>,
    host_resume_tokens: BTreeMap<NodeKey, String>,
    runtime_resume_tokens: BTreeMap<RuntimeKey, String>,
    runtime_grants: BTreeMap<RuntimeKey, EnrollmentGrant>,
    pending_runtime_enrollments: BTreeMap<RuntimeKey, PendingRuntimeEnrollment>,
    runtime_node_bindings: BTreeMap<semantic::Identity, NodeKey>,
    highest_node_epochs: BTreeMap<String, u64>,
    accepted_assignments: BTreeMap<String, AssignmentBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RuntimeKey {
    runtime: semantic::Identity,
    node: NodeKey,
}

#[derive(Debug, Clone)]
struct PendingRuntimeEnrollment {
    proof_digest: [u8; 32],
    grant: EnrollmentGrant,
    resume_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionBinding {
    Host {
        node: NodeKey,
        session_id: String,
    },
    Runtime {
        runtime: semantic::Identity,
        node: NodeKey,
        session_id: String,
    },
}

impl SessionBinding {
    fn for_identity(identity: &SessionIdentity, session_id: &str) -> Self {
        match identity {
            SessionIdentity::Host(node) => Self::Host {
                node: node.clone(),
                session_id: session_id.to_string(),
            },
            SessionIdentity::Runtime(runtime, node) => Self::Runtime {
                runtime: runtime.clone(),
                node: node.clone(),
                session_id: session_id.to_string(),
            },
        }
    }

    fn session_id(&self) -> &str {
        match self {
            Self::Host { session_id, .. } | Self::Runtime { session_id, .. } => session_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LeaseKey {
    identity: semantic::Identity,
    fence_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssignmentBinding {
    assignment_id: String,
    runtime: semantic::Identity,
    node: NodeKey,
    lease: LeaseKey,
    digest: [u8; 32],
}

impl Registry {
    fn check_node_epoch(&self, node: &NodeKey) -> Result<(), DispatchError> {
        if self
            .highest_node_epochs
            .get(&node.node_id)
            .is_some_and(|highest| node.node_epoch < *highest)
        {
            return Err(DispatchError::input(
                "STALE_NODE_GENERATION",
                "NodeRef epoch is lower than the highest accepted generation",
            ));
        }
        Ok(())
    }

    fn admit_node_epoch(
        &mut self,
        node: &NodeKey,
    ) -> Result<Vec<Arc<SessionHandle>>, DispatchError> {
        if let Some(highest) = self.highest_node_epochs.get(&node.node_id) {
            self.check_node_epoch(node)?;
            if node.node_epoch == *highest {
                return Ok(Vec::new());
            }
        }
        self.highest_node_epochs
            .insert(node.node_id.clone(), node.node_epoch);

        let old_host_keys = self
            .hosts
            .keys()
            .filter(|key| key.node_id == node.node_id && key.node_epoch < node.node_epoch)
            .cloned()
            .collect::<Vec<_>>();
        let old_runtime_keys = self
            .runtimes
            .keys()
            .filter(|key| key.node.node_id == node.node_id && key.node.node_epoch < node.node_epoch)
            .cloned()
            .collect::<Vec<_>>();
        let mut evicted = Vec::new();
        for key in old_host_keys {
            if let Some(session) = self.hosts.remove(&key) {
                evicted.push(session.handle);
            }
            self.host_resume_tokens.remove(&key);
        }
        for key in old_runtime_keys {
            if let Some(session) = self.runtimes.remove(&key) {
                evicted.push(session.handle);
            }
            self.runtime_resume_tokens.remove(&key);
            self.runtime_grants.remove(&key);
            self.pending_runtime_enrollments.remove(&key);
        }
        self.accepted_assignments.retain(|_, binding| {
            binding.node.node_id != node.node_id || binding.node.node_epoch >= node.node_epoch
        });
        Ok(evicted)
    }

    fn validate_runtime_attachment(
        &self,
        runtime: &semantic::Identity,
        node: &NodeKey,
    ) -> Result<(), DispatchError> {
        self.check_node_epoch(node)?;
        if self
            .runtime_node_bindings
            .get(runtime)
            .is_some_and(|bound| bound != node)
        {
            return Err(DispatchError::input(
                "RUNTIME_NODE_REBIND_REJECTED",
                "a Runtime generation cannot be rebound to a different NodeKey",
            ));
        }
        if self.highest_node_epochs.get(&node.node_id) != Some(&node.node_epoch)
            || self
                .hosts
                .get(node)
                .is_none_or(|session| !session.handle.is_usable())
        {
            return Err(DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "Runtime Agent requires an authenticated Host session for the exact NodeKey",
            ));
        }
        Ok(())
    }
}

struct PendingCommand {
    binding: SessionBinding,
    sender: oneshot::Sender<KernelCommandResult>,
}

struct PendingAssignment {
    binding: SessionBinding,
    assignment: AssignmentBinding,
    sender: oneshot::Sender<AssignmentAck>,
}

#[derive(Clone)]
pub(crate) struct HostRouteSnapshot {
    node: NodeKey,
    session_id: String,
    handle: Arc<SessionHandle>,
}

#[derive(Clone)]
pub(crate) struct RuntimeRouteSnapshot {
    runtime: semantic::Identity,
    node: NodeKey,
    session_id: String,
    handle: Arc<SessionHandle>,
}

/// One atomic view of the Host and Runtime sessions used for one execution.
/// The view is revalidated immediately before every outbound send.
#[derive(Clone)]
pub(crate) struct RouteSnapshot {
    pub host: HostRouteSnapshot,
    pub runtime: RuntimeRouteSnapshot,
}

struct Inner {
    authenticator: Arc<dyn PeerAuthenticator>,
    enrollment: Arc<dyn EnrollmentProvider>,
    heartbeat_interval: Duration,
    authority_response_timeout: Duration,
    session_admission_gate: AsyncMutex<()>,
    registry: Mutex<Registry>,
    pending_commands: Mutex<BTreeMap<String, PendingCommand>>,
    pending_assignments: Mutex<BTreeMap<String, PendingAssignment>>,
    observations: broadcast::Sender<ControlObservation>,
}

/// Canonical Rust implementation of the existing NodeControl protocol seam.
///
/// The service stores only authenticated session/correlation state. It never
/// stores Resource allocations, Leases, fences, Provider facts, or Product
/// Run/Attempt state.
#[derive(Clone)]
pub struct ExecutionControlService {
    inner: Arc<Inner>,
}

impl ExecutionControlService {
    pub fn new(
        authenticator: Arc<dyn PeerAuthenticator>,
        enrollment: Arc<dyn EnrollmentProvider>,
        heartbeat_interval: Duration,
        authority_response_timeout: Duration,
    ) -> Result<Self, DispatchError> {
        if heartbeat_interval.is_zero() || authority_response_timeout.is_zero() {
            return Err(DispatchError::input(
                "CONTROL_INTERVAL_INVALID",
                "NodeControl heartbeat interval and authority timeout must be positive",
            ));
        }
        let (observations, _) = broadcast::channel(1024);
        Ok(Self {
            inner: Arc::new(Inner {
                authenticator,
                enrollment,
                heartbeat_interval,
                authority_response_timeout,
                session_admission_gate: AsyncMutex::new(()),
                registry: Mutex::new(Registry::default()),
                pending_commands: Mutex::new(BTreeMap::new()),
                pending_assignments: Mutex::new(BTreeMap::new()),
                observations,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ControlObservation> {
        self.inner.observations.subscribe()
    }

    pub fn has_host_session(&self, node: &core_v1::NodeRef) -> bool {
        let key = NodeKey::from(node);
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        registry.highest_node_epochs.get(&key.node_id) == Some(&key.node_epoch)
            && registry.hosts.contains_key(&key)
            && registry
                .hosts
                .get(&key)
                .is_some_and(|session| session.handle.is_usable())
    }

    pub fn has_runtime_session(&self, runtime: &semantic::Identity) -> bool {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        registry.runtimes.iter().any(|(key, session)| {
            key.runtime == *runtime
                && registry.highest_node_epochs.get(&key.node.node_id) == Some(&key.node.node_epoch)
                && session.handle.is_usable()
        })
    }

    /// Project the exact enrollment grant into one assignment-scoped
    /// workload identity. The caller supplies only user context and bounded
    /// actions; it cannot replace Runtime, Workspace, or expiry authority.
    pub fn workload_identity(
        &self,
        runtime: &semantic::Identity,
        user_id: impl Into<String>,
        allowed_actions: Vec<String>,
    ) -> Result<core_v1::WorkloadIdentity, DispatchError> {
        let user_id = user_id.into();
        validate_user_and_actions(&user_id, &allowed_actions)?;
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let session = registry
            .runtimes
            .iter()
            .find_map(|(key, session)| {
                (key.runtime == *runtime
                    && registry.highest_node_epochs.get(&key.node.node_id)
                        == Some(&key.node.node_epoch)
                    && session.handle.is_usable())
                .then_some(session)
            })
            .ok_or_else(|| {
                DispatchError::transient(
                    "RUNTIME_SESSION_UNAVAILABLE",
                    "Runtime generation has no authenticated execution Agent session",
                )
            })?;
        Ok(core_v1::WorkloadIdentity {
            identity: Some(identity_to_proto(&session.workload_identity)),
            scope: Some(core_v1::AccountScope {
                user_id,
                organization_id: session.organization_id.clone(),
                workspace_id: session.workspace_id.clone(),
            }),
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(identity_to_proto(runtime)),
            }),
            allowed_actions,
            expires_at: Some(timestamp_from_unix_ms(
                session.workload_identity_expires_at_unix_ms,
            )),
        })
    }

    /// Capture the exact Host/Runtime pair for one dispatch decision while
    /// holding one registry lock. Callers must pass this snapshot to the
    /// outbound helpers; they never re-resolve a Node or Runtime separately.
    pub(crate) fn snapshot_route(
        &self,
        node: &core_v1::NodeRef,
        runtime: &semantic::Identity,
    ) -> Result<RouteSnapshot, DispatchError> {
        let key = NodeKey::from(node);
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        if registry.highest_node_epochs.get(&key.node_id) != Some(&key.node_epoch) {
            return Err(DispatchError::input(
                "STALE_NODE_GENERATION",
                "selected NodeRef is not the highest accepted Node generation",
            ));
        }
        let host = registry.hosts.get(&key).ok_or_else(|| {
            DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "selected Node has no authenticated Host Agent session",
            )
        })?;
        if !host.handle.is_usable() {
            return Err(DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "selected Host Agent session has been fenced",
            ));
        }
        let (runtime_key, runtime_session) = registry
            .runtimes
            .iter()
            .find(|(runtime_key, session)| {
                runtime_key.runtime == *runtime
                    && runtime_key.node == key
                    && session.handle.is_usable()
            })
            .ok_or_else(|| {
                DispatchError::transient(
                    "RUNTIME_SESSION_UNAVAILABLE",
                    "Runtime generation has no authenticated session on the selected Node",
                )
            })?;
        Ok(RouteSnapshot {
            host: HostRouteSnapshot {
                node: key.clone(),
                session_id: host.handle.session_id.clone(),
                handle: host.handle.clone(),
            },
            runtime: RuntimeRouteSnapshot {
                runtime: runtime_key.runtime.clone(),
                node: runtime_key.node.clone(),
                session_id: runtime_session.handle.session_id.clone(),
                handle: runtime_session.handle.clone(),
            },
        })
    }

    pub(crate) fn snapshot_host_route(
        &self,
        node: &core_v1::NodeRef,
    ) -> Result<HostRouteSnapshot, DispatchError> {
        let key = NodeKey::from(node);
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        if registry.highest_node_epochs.get(&key.node_id) != Some(&key.node_epoch) {
            return Err(DispatchError::input(
                "STALE_NODE_GENERATION",
                "selected NodeRef is not the highest accepted Node generation",
            ));
        }
        let session = registry.hosts.get(&key).ok_or_else(|| {
            DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "selected Node has no authenticated Host Agent session",
            )
        })?;
        if !session.handle.is_usable() {
            return Err(DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "selected Host Agent session has been fenced",
            ));
        }
        Ok(HostRouteSnapshot {
            node: key,
            session_id: session.handle.session_id.clone(),
            handle: session.handle.clone(),
        })
    }

    pub(crate) fn runtime_session_on_route(
        &self,
        route: &RouteSnapshot,
    ) -> Result<RuntimeSessionView, DispatchError> {
        if !self.route_is_current(route) {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "captured Host/Runtime route is no longer current",
            ));
        }
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let key = RuntimeKey {
            runtime: route.runtime.runtime.clone(),
            node: route.runtime.node.clone(),
        };
        let session = registry.runtimes.get(&key).ok_or_else(|| {
            DispatchError::transient(
                "RUNTIME_SESSION_UNAVAILABLE",
                "captured Runtime route has no authenticated session",
            )
        })?;
        if session.handle.session_id != route.runtime.session_id || !session.handle.is_usable() {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "captured Runtime route session has been replaced",
            ));
        }
        Ok(RuntimeSessionView {
            node: session.node.to_proto(),
            organization_id: session.organization_id.clone(),
            workspace_id: session.workspace_id.clone(),
            workload_identity: session.workload_identity.clone(),
            workload_identity_expires_at_unix_ms: session.workload_identity_expires_at_unix_ms,
        })
    }

    fn host_route_is_current(&self, route: &HostRouteSnapshot) -> bool {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        registry.highest_node_epochs.get(&route.node.node_id) == Some(&route.node.node_epoch)
            && registry.hosts.get(&route.node).is_some_and(|session| {
                session.handle.session_id == route.session_id && session.handle.is_usable()
            })
    }

    fn runtime_route_is_current(&self, route: &RuntimeRouteSnapshot) -> bool {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let key = RuntimeKey {
            runtime: route.runtime.clone(),
            node: route.node.clone(),
        };
        registry.runtimes.get(&key).is_some_and(|session| {
            registry.highest_node_epochs.get(&route.node.node_id) == Some(&route.node.node_epoch)
                && session.handle.session_id == route.session_id
                && session.handle.is_usable()
        })
    }

    fn route_is_current(&self, route: &RouteSnapshot) -> bool {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let host_current = registry.highest_node_epochs.get(&route.host.node.node_id)
            == Some(&route.host.node.node_epoch)
            && registry.hosts.get(&route.host.node).is_some_and(|session| {
                session.handle.session_id == route.host.session_id && session.handle.is_usable()
            });
        let runtime_key = RuntimeKey {
            runtime: route.runtime.runtime.clone(),
            node: route.runtime.node.clone(),
        };
        let runtime_current = registry.runtimes.get(&runtime_key).is_some_and(|session| {
            registry
                .highest_node_epochs
                .get(&route.runtime.node.node_id)
                == Some(&route.runtime.node.node_epoch)
                && session.handle.session_id == route.runtime.session_id
                && session.handle.is_usable()
        });
        route.host.node == route.runtime.node && host_current && runtime_current
    }

    pub(crate) async fn acquire_lease_on_route(
        &self,
        route: &RouteSnapshot,
        acquisition: LeaseAcquisition<'_>,
    ) -> Result<semantic_v1::Lease, DispatchError> {
        if !self.route_is_current(route)
            || route.host.node != NodeKey::from(acquisition.node)
            || route.runtime.runtime != *acquisition.holder
        {
            return Err(DispatchError::input(
                "ROUTE_IDENTITY_MISMATCH",
                "Lease acquisition does not match its captured Host/Runtime route",
            ));
        }
        let command = KernelCommand {
            command_id: acquisition.command_id.to_string(),
            request: Some(kernel_command::Request::Authority(KernelAuthorityCommand {
                request: Some(kernel_authority_command::Request::AcquireLease(
                    core_v1::AcquireLeaseRequest {
                        context: Some(context_to_proto(acquisition.context)),
                        holder: Some(identity_to_proto(acquisition.holder)),
                        query: Some(resource_query_to_proto(acquisition.query)),
                        ttl: Some(duration_to_proto(acquisition.ttl)?),
                    },
                )),
            })),
        };
        let result = self
            .send_kernel_command_on_route(&route.host, command, acquisition.response_timeout)
            .await?;
        authority_lease_result(result)
    }

    pub(crate) async fn release_lease(
        &self,
        node: &core_v1::NodeRef,
        lease: &semantic::Lease,
        command_id: &str,
        context: &AuthorityCallContext,
        response_timeout: Duration,
    ) -> Result<(), DispatchError> {
        let deadline = tokio::time::Instant::now() + response_timeout;
        let route = loop {
            match self.snapshot_host_route(node) {
                Ok(route) => break route,
                Err(error)
                    if error.reason_code == "HOST_SESSION_UNAVAILABLE"
                        && tokio::time::Instant::now() < deadline =>
                {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
                }
                Err(error) => return Err(error),
            }
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DispatchError::transient(
                "HOST_SESSION_UNAVAILABLE",
                "Host session did not become ready before the release deadline",
            ));
        }
        self.release_lease_on_route(&route, lease, command_id, context, remaining)
            .await
    }

    pub(crate) async fn release_lease_on_route(
        &self,
        route: &HostRouteSnapshot,
        lease: &semantic::Lease,
        command_id: &str,
        context: &AuthorityCallContext,
        response_timeout: Duration,
    ) -> Result<(), DispatchError> {
        let command = KernelCommand {
            command_id: command_id.to_string(),
            request: Some(kernel_command::Request::Authority(KernelAuthorityCommand {
                request: Some(kernel_authority_command::Request::ReleaseLease(
                    core_v1::ReleaseLeaseRequest {
                        context: Some(context_to_proto(context)),
                        lease: Some(identity_to_proto(&lease.identity)),
                        fence_token: lease.fence_token,
                    },
                )),
            })),
        };
        let result = self
            .send_kernel_command_on_route(route, command, response_timeout)
            .await?;
        let released = authority_lease_result(result)?;
        let released_semantic = semantic_lease_from_proto(&released).map_err(|error| {
            DispatchError::unknown(format!(
                "Kernel returned an invalid Lease after release: {error}"
            ))
        })?;
        if released_semantic.identity != lease.identity
            || released_semantic.holder != lease.holder
            || released_semantic.resources != lease.resources
            || released_semantic.fence_token != lease.fence_token
        {
            return Err(DispatchError::unknown(
                "Kernel release returned a different Lease identity or fence",
            ));
        }
        if released_semantic.state != semantic::LeaseState::Released {
            return Err(DispatchError::unknown(format!(
                "Kernel release returned non-terminal Lease state {:?}",
                released_semantic.state
            )));
        }
        self.remove_assignment_bindings(&LeaseKey {
            identity: lease.identity.clone(),
            fence_token: lease.fence_token,
        });
        Ok(())
    }

    async fn renew_lease(
        &self,
        node: &core_v1::NodeRef,
        runtime: &semantic::Identity,
        request: &LeaseRenewalRequest,
    ) -> Result<semantic_v1::Lease, DispatchError> {
        if request.request_id.is_empty() {
            return Err(DispatchError::input(
                "RENEWAL_REQUEST_ID_REQUIRED",
                "Lease renewal request id is required",
            ));
        }
        let lease = request.lease.as_ref().ok_or_else(|| {
            DispatchError::input(
                "LEASE_IDENTITY_REQUIRED",
                "Lease renewal requires a Lease identity",
            )
        })?;
        let route = self.snapshot_route(node, runtime)?;
        let requested_runtime = request
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.identity.as_ref())
            .ok_or_else(|| {
                DispatchError::input(
                    "RENEWAL_RUNTIME_REQUIRED",
                    "Lease renewal requires its Runtime identity",
                )
            })
            .and_then(semantic_identity_from_proto)?;
        if requested_runtime != *runtime {
            return Err(DispatchError::input(
                "RENEWAL_RUNTIME_MISMATCH",
                "Lease renewal Runtime does not match the authenticated Runtime session",
            ));
        }
        let lease_identity = semantic_identity_from_proto(lease)?;
        self.require_assignment_lease_binding(
            runtime,
            &route.runtime.node,
            &LeaseKey {
                identity: lease_identity,
                fence_token: request.fence_token,
            },
        )?;
        let requested_expiry = request
            .requested_expires_at
            .as_ref()
            .and_then(timestamp_unix_ms)
            .ok_or_else(|| {
                DispatchError::input(
                    "LEASE_EXPIRY_INVALID",
                    "Lease renewal requires an exact positive millisecond expiry",
                )
            })?;
        let now = now_unix_ms();
        let ttl = Duration::from_millis(requested_expiry.checked_sub(now).ok_or_else(|| {
            DispatchError::input(
                "LEASE_EXPIRY_INVALID",
                "Lease renewal expiry must be in the future",
            )
        })?);
        let context = AuthorityCallContext {
            contract: semantic::ContractRevision::current(),
            namespace: cy_kernel_api::NamespaceId::default(),
            request_id: request.request_id.clone(),
            idempotency_key: request.request_id.clone(),
        };
        let command_id = format!(
            "renew-{}-{}-{}",
            runtime.id, runtime.generation, request.request_id
        );
        let command = KernelCommand {
            command_id,
            request: Some(kernel_command::Request::Authority(KernelAuthorityCommand {
                request: Some(kernel_authority_command::Request::RenewLease(
                    core_v1::RenewLeaseRequest {
                        context: Some(context_to_proto(&context)),
                        lease: Some(lease.clone()),
                        fence_token: request.fence_token,
                        ttl: Some(duration_to_proto(ttl)?),
                    },
                )),
            })),
        };
        let result = self
            .send_kernel_command_on_route(
                &route.host,
                command,
                self.inner.authority_response_timeout,
            )
            .await?;
        authority_lease_result(result)
    }

    pub(crate) async fn dispatch_assignment_on_route(
        &self,
        route: &RuntimeRouteSnapshot,
        assignment: core_v1::RuntimeAssignment,
        response_timeout: Duration,
    ) -> Result<AssignmentAck, DispatchError> {
        if !self.runtime_route_is_current(route) {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "Runtime route changed before assignment delivery",
            ));
        }
        let assignment_binding = self.assignment_binding(route, &assignment)?;
        let assignment_id = assignment.assignment_id.clone();
        let expected = SessionBinding::Runtime {
            runtime: route.runtime.clone(),
            node: route.node.clone(),
            session_id: route.session_id.clone(),
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .pending_assignments
                .lock()
                .expect("pending assignment lock poisoned");
            if pending.contains_key(&assignment_id) {
                return Err(DispatchError::input(
                    "ASSIGNMENT_CORRELATION_DUPLICATE",
                    "assignment id already has a pending delivery",
                ));
            }
            pending.insert(
                assignment_id.clone(),
                PendingAssignment {
                    binding: expected.clone(),
                    assignment: assignment_binding,
                    sender,
                },
            );
        }
        if !self.runtime_route_is_current(route) {
            self.remove_pending_assignment(&assignment_id, &expected);
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "Runtime route changed before assignment send",
            ));
        }
        if let Err(error) = route
            .handle
            .send(control_plane_to_node::Body::RuntimeAssignment(assignment))
            .await
        {
            self.remove_pending_assignment(&assignment_id, &expected);
            return Err(error);
        }
        match tokio::time::timeout(response_timeout, receiver).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) | Err(_) => {
                self.remove_pending_assignment(&assignment_id, &expected);
                Err(DispatchError::unknown(format!(
                    "no definitive AssignmentAck for {assignment_id}"
                )))
            }
        }
    }

    async fn send_kernel_command_on_route(
        &self,
        route: &HostRouteSnapshot,
        command: KernelCommand,
        response_timeout: Duration,
    ) -> Result<KernelCommandResult, DispatchError> {
        if !self.host_route_is_current(route) {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "Host route changed before authority command delivery",
            ));
        }
        let command_id = command.command_id.clone();
        let expected = SessionBinding::Host {
            node: route.node.clone(),
            session_id: route.session_id.clone(),
        };
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .inner
                .pending_commands
                .lock()
                .expect("pending command lock poisoned");
            if pending.contains_key(&command_id) {
                return Err(DispatchError::input(
                    "COMMAND_CORRELATION_DUPLICATE",
                    "command id already has a pending authority request",
                ));
            }
            pending.insert(
                command_id.clone(),
                PendingCommand {
                    binding: expected.clone(),
                    sender,
                },
            );
        }
        if !self.host_route_is_current(route) {
            self.remove_pending_command(&command_id, &expected);
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "Host route changed before authority command send",
            ));
        }
        if let Err(error) = route
            .handle
            .send(control_plane_to_node::Body::Command(command))
            .await
        {
            self.remove_pending_command(&command_id, &expected);
            return Err(error);
        }
        match tokio::time::timeout(response_timeout, receiver).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) | Err(_) => {
                self.remove_pending_command(&command_id, &expected);
                Err(DispatchError::unknown(format!(
                    "no definitive Kernel authority result for command {command_id}"
                )))
            }
        }
    }

    fn remove_pending_command(&self, command_id: &str, expected: &SessionBinding) {
        let mut pending = self
            .inner
            .pending_commands
            .lock()
            .expect("pending command lock poisoned");
        if pending
            .get(command_id)
            .is_some_and(|entry| &entry.binding == expected)
        {
            pending.remove(command_id);
        }
    }

    fn remove_pending_assignment(&self, assignment_id: &str, expected: &SessionBinding) {
        let mut pending = self
            .inner
            .pending_assignments
            .lock()
            .expect("pending assignment lock poisoned");
        if pending
            .get(assignment_id)
            .is_some_and(|entry| &entry.binding == expected)
        {
            pending.remove(assignment_id);
        }
    }

    fn assignment_binding(
        &self,
        route: &RuntimeRouteSnapshot,
        assignment: &core_v1::RuntimeAssignment,
    ) -> Result<AssignmentBinding, DispatchError> {
        if assignment.assignment_id.is_empty() {
            return Err(DispatchError::input(
                "ASSIGNMENT_ID_REQUIRED",
                "Runtime assignment id is required",
            ));
        }
        let assignment_runtime = assignment
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.identity.as_ref())
            .map(|identity| semantic::Identity {
                id: identity.id.clone(),
                generation: identity.generation,
            })
            .ok_or_else(|| {
                DispatchError::input(
                    "ASSIGNMENT_RUNTIME_REQUIRED",
                    "Runtime assignment requires a Runtime identity",
                )
            })?;
        if assignment_runtime != route.runtime {
            return Err(DispatchError::input(
                "ASSIGNMENT_RUNTIME_MISMATCH",
                "Runtime assignment identity does not match the authenticated Runtime session",
            ));
        }
        let workload = assignment.workload_identity.as_ref();
        validate_workload_identity_payload(workload)?;
        let workload_runtime = workload
            .and_then(|workload| workload.runtime.as_ref())
            .and_then(|runtime| runtime.identity.as_ref())
            .ok_or_else(|| {
                DispatchError::input(
                    "WORKLOAD_RUNTIME_REQUIRED",
                    "Runtime assignment workload identity requires its Runtime identity",
                )
            })
            .and_then(semantic_identity_from_proto)?;
        if workload_runtime != route.runtime {
            return Err(DispatchError::input(
                "WORKLOAD_RUNTIME_MISMATCH",
                "workload identity Runtime does not match the authenticated Runtime session",
            ));
        }
        let lease_proto = assignment.lease.as_ref().ok_or_else(|| {
            DispatchError::input(
                "ASSIGNMENT_LEASE_REQUIRED",
                "Runtime assignment requires a canonical Lease projection",
            )
        })?;
        let lease = semantic_lease_from_proto(lease_proto).map_err(|error| {
            DispatchError::input(
                error.reason_code,
                format!("Runtime assignment Lease is invalid: {}", error.message),
            )
        })?;
        if lease.state != semantic::LeaseState::Active {
            return Err(DispatchError::input(
                "ASSIGNMENT_LEASE_NOT_ACTIVE",
                "Runtime assignment must carry an active canonical Lease",
            ));
        }
        if lease
            .expires_at_unix_ms
            .is_none_or(|expires_at| now_unix_ms() >= expires_at)
        {
            return Err(DispatchError::input(
                "ASSIGNMENT_LEASE_EXPIRED",
                "Runtime assignment Lease has already expired",
            ));
        }
        if lease.holder != route.runtime {
            return Err(DispatchError::input(
                "ASSIGNMENT_LEASE_HOLDER_MISMATCH",
                "Runtime assignment Lease holder does not match the authenticated Runtime",
            ));
        }
        let binding = AssignmentBinding {
            assignment_id: assignment.assignment_id.clone(),
            runtime: route.runtime.clone(),
            node: route.node.clone(),
            lease: LeaseKey {
                identity: lease.identity,
                fence_token: lease.fence_token,
            },
            digest: Sha256::digest(assignment.encode_to_vec()).into(),
        };
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        if registry
            .accepted_assignments
            .get(&binding.assignment_id)
            .is_some_and(|existing| existing != &binding)
        {
            return Err(DispatchError::input(
                "ASSIGNMENT_ID_REUSED",
                "assignment id is already bound to a different Runtime, Node, Lease, or payload",
            ));
        }
        Ok(binding)
    }

    fn complete_command_result(
        &self,
        identity: &SessionIdentity,
        session_id: &str,
        result: &KernelCommandResult,
    ) -> Result<(), DispatchError> {
        let expected = SessionBinding::for_identity(identity, session_id);
        let sender = {
            let mut pending = self
                .inner
                .pending_commands
                .lock()
                .expect("pending command lock poisoned");
            let Some(entry) = pending.get(&result.command_id) else {
                return Ok(());
            };
            if entry.binding != expected {
                return Err(DispatchError::input(
                    "PENDING_COMMAND_SESSION_MISMATCH",
                    "Kernel authority result came from the wrong authenticated session",
                ));
            }
            pending
                .remove(&result.command_id)
                .expect("pending command entry disappeared while locked")
                .sender
        };
        let _ = sender.send(result.clone());
        Ok(())
    }

    fn complete_assignment_ack(
        &self,
        identity: &SessionIdentity,
        session_id: &str,
        ack: &AssignmentAck,
    ) -> Result<(), DispatchError> {
        let expected = SessionBinding::for_identity(identity, session_id);
        let mut pending = self
            .inner
            .pending_assignments
            .lock()
            .expect("pending assignment lock poisoned");
        let Some(entry) = pending.get(&ack.assignment_id) else {
            return Ok(());
        };
        if entry.binding != expected {
            return Err(DispatchError::input(
                "PENDING_ASSIGNMENT_SESSION_MISMATCH",
                "Assignment acknowledgement came from the wrong authenticated session",
            ));
        }
        let disposition = AssignmentAckDisposition::try_from(ack.disposition).map_err(|_| {
            DispatchError::input(
                "ASSIGNMENT_ACK_INVALID",
                "Assignment acknowledgement has an unknown disposition",
            )
        })?;
        if matches!(
            disposition,
            AssignmentAckDisposition::Accepted | AssignmentAckDisposition::Duplicate
        ) {
            let binding = entry.assignment.clone();
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            if registry
                .accepted_assignments
                .get(&binding.assignment_id)
                .is_some_and(|existing| existing != &binding)
            {
                return Err(DispatchError::input(
                    "ASSIGNMENT_ID_REUSED",
                    "assignment acknowledgement conflicts with the existing accepted binding",
                ));
            }
            registry
                .accepted_assignments
                .insert(binding.assignment_id.clone(), binding);
        }
        let sender = pending
            .remove(&ack.assignment_id)
            .expect("pending assignment entry disappeared while locked")
            .sender;
        let _ = sender.send(ack.clone());
        Ok(())
    }

    fn require_assignment_lease_binding(
        &self,
        runtime: &semantic::Identity,
        node: &NodeKey,
        lease: &LeaseKey,
    ) -> Result<(), DispatchError> {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let binding = registry
            .accepted_assignments
            .values()
            .find(|binding| &binding.lease == lease)
            .ok_or_else(|| {
                DispatchError::input(
                    "LEASE_ASSIGNMENT_BINDING_REQUIRED",
                    "Lease renewal is allowed only for an accepted Runtime assignment",
                )
            })?;
        if &binding.runtime != runtime || &binding.node != node {
            return Err(DispatchError::input(
                "LEASE_ASSIGNMENT_BINDING_MISMATCH",
                "Lease renewal Runtime or Node does not match its accepted assignment",
            ));
        }
        Ok(())
    }

    fn remove_assignment_bindings(&self, lease: &LeaseKey) {
        self.inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned")
            .accepted_assignments
            .retain(|_, binding| &binding.lease != lease);
    }

    async fn fence_sessions(&self, sessions: Vec<Arc<SessionHandle>>, reason: &'static str) {
        let session_ids = sessions
            .iter()
            .map(|session| session.session_id.clone())
            .collect::<BTreeSet<_>>();
        self.remove_pending_for_sessions(&session_ids);
        for session in sessions {
            session.fence().await;
            session.close_with(Status::aborted(reason)).await;
        }
    }

    fn remove_pending_for_sessions(&self, session_ids: &BTreeSet<String>) {
        self.inner
            .pending_commands
            .lock()
            .expect("pending command lock poisoned")
            .retain(|_, pending| !session_ids.contains(pending.binding.session_id()));
        self.inner
            .pending_assignments
            .lock()
            .expect("pending assignment lock poisoned")
            .retain(|_, pending| !session_ids.contains(pending.binding.session_id()));
    }

    async fn establish(
        &self,
        peer: AuthenticatedAgent,
        first: NodeToControlPlane,
        sender: mpsc::Sender<OutboundItem>,
    ) -> Result<(Arc<SessionHandle>, SessionIdentity), DispatchError> {
        if first.sequence_number != 1 || !first.session_id.is_empty() || first.frame_id.is_empty() {
            return Err(DispatchError::input(
                "HELLO_FRAME_INVALID",
                "initial Agent frame requires sequence 1, no session id, and a frame id",
            ));
        }
        match (peer, first.body.as_ref()) {
            (
                AuthenticatedAgent::Host { node },
                Some(node_to_control_plane::Body::Hello(hello)),
            ) => self.establish_host(node, hello, sender).await,
            (
                AuthenticatedAgent::Runtime { runtime, node },
                Some(node_to_control_plane::Body::ExecutionAgentHello(hello)),
            ) => self.establish_runtime(runtime, node, hello, sender).await,
            _ => Err(DispatchError::input(
                "AGENT_ROLE_MISMATCH",
                "authenticated Agent role does not match the initial Hello body",
            )),
        }
    }

    async fn establish_host(
        &self,
        authenticated_node: core_v1::NodeRef,
        hello: &NodeHello,
        sender: mpsc::Sender<OutboundItem>,
    ) -> Result<(Arc<SessionHandle>, SessionIdentity), DispatchError> {
        let _admission = self.inner.session_admission_gate.lock().await;
        let node = hello.node.as_ref().ok_or_else(|| {
            DispatchError::input("NODE_IDENTITY_REQUIRED", "Host Hello requires NodeRef")
        })?;
        if node != &authenticated_node || node.node_id.is_empty() || node.node_epoch == 0 {
            return Err(DispatchError::input(
                "AUTHENTICATED_NODE_MISMATCH",
                "Host certificate identity does not exactly match the Hello NodeRef",
            ));
        }
        if hello.agent_version.is_empty()
            || hello.min_protocol_version == 0
            || hello.min_protocol_version > 1
            || hello.max_protocol_version < 1
        {
            return Err(DispatchError::input(
                "HOST_PROTOCOL_INCOMPATIBLE",
                "Host Agent protocol range must include Core v1",
            ));
        }
        if !hello.offered_contracts.iter().any(supported_contract) {
            return Err(DispatchError::input(
                "CONTRACT_INCOMPATIBLE",
                "Host Agent did not offer the canonical semantic contract",
            ));
        }
        let key = NodeKey::from(node);
        let session_id = Uuid::new_v4().to_string();
        let resume_token = Uuid::new_v4().to_string();
        let handle = Arc::new(SessionHandle::new(session_id.clone(), sender));
        let session = HostSession {
            handle: handle.clone(),
        };
        let (mut evicted, replaced) = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            if !hello.resume_token.is_empty()
                && registry.host_resume_tokens.get(&key) != Some(&hello.resume_token)
            {
                return Err(DispatchError::input(
                    "RESUME_TOKEN_REJECTED",
                    "Host Agent resume token is invalid for this Node incarnation",
                ));
            }
            let evicted = registry.admit_node_epoch(&key)?;
            registry
                .host_resume_tokens
                .insert(key.clone(), resume_token.clone());
            (evicted, registry.hosts.insert(key.clone(), session))
        };
        if let Some(replaced) = replaced {
            evicted.push(replaced.handle);
        }
        self.fence_sessions(
            evicted,
            "Host session replaced or fenced by a newer authenticated connection",
        )
        .await;
        handle
            .send_welcome(control_plane_to_node::Body::Welcome(core_v1::NodeWelcome {
                session_id: session_id.clone(),
                selected_protocol_version: 1,
                desired_generation: node.node_epoch,
                heartbeat_interval: Some(duration_to_proto(self.inner.heartbeat_interval)?),
                server_time: Some(now_timestamp()),
                resume_token,
                selected_contract: Some(contract_to_proto()),
            }))
            .await?;
        Ok((handle, SessionIdentity::Host(key)))
    }

    async fn establish_runtime(
        &self,
        authenticated_runtime: semantic::Identity,
        authenticated_node: core_v1::NodeRef,
        hello: &ExecutionAgentHello,
        sender: mpsc::Sender<OutboundItem>,
    ) -> Result<(Arc<SessionHandle>, SessionIdentity), DispatchError> {
        let _admission = self.inner.session_admission_gate.lock().await;
        validate_hello(hello)?;
        let runtime = runtime_identity(hello)?;
        let node = hello
            .node
            .as_ref()
            .and_then(|descriptor| descriptor.node.as_ref())
            .expect("validated Runtime NodeRef");
        if runtime != authenticated_runtime || node != &authenticated_node {
            return Err(DispatchError::input(
                "AUTHENTICATED_RUNTIME_MISMATCH",
                "Runtime certificate identity does not exactly match ExecutionAgentHello",
            ));
        }
        let scope = hello.scope.as_ref().expect("validated Runtime scope");
        let expected_scope = RuntimeScope {
            organization_id: scope.organization_id.clone(),
            workspace_id: scope.workspace_id.clone(),
            runtime: runtime.clone(),
        };
        let proof_digest: [u8; 32] = Sha256::digest(hello.enrollment_proof.as_bytes()).into();
        let key = NodeKey::from(node);
        let runtime_key = RuntimeKey {
            runtime: runtime.clone(),
            node: key.clone(),
        };
        let retained_credential = {
            let registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            registry.validate_runtime_attachment(&runtime, &key)?;
            if hello.resume_token.is_empty() {
                if let Some(pending) = registry.pending_runtime_enrollments.get(&runtime_key) {
                    if pending.proof_digest != proof_digest {
                        return Err(DispatchError::input(
                            "RESUME_TOKEN_REQUIRED",
                            "an enrolled Runtime generation must present its current resume token",
                        ));
                    }
                    Some((pending.grant.clone(), pending.resume_token.clone()))
                } else {
                    if registry.runtime_resume_tokens.contains_key(&runtime_key) {
                        return Err(DispatchError::input(
                            "RESUME_TOKEN_REQUIRED",
                            "an enrolled Runtime generation must present its current resume token",
                        ));
                    }
                    None
                }
            } else {
                if registry.runtime_resume_tokens.get(&runtime_key) != Some(&hello.resume_token) {
                    return Err(DispatchError::input(
                        "RESUME_TOKEN_REJECTED",
                        "Runtime Agent resume token is invalid for this Runtime and NodeKey",
                    ));
                }
                let grant = registry
                    .runtime_grants
                    .get(&runtime_key)
                    .cloned()
                    .ok_or_else(|| {
                        DispatchError::input(
                            "RESUME_STATE_UNAVAILABLE",
                            "Runtime resume token has no retained enrollment grant",
                        )
                    })?;
                Some((grant, hello.resume_token.clone()))
            }
        };
        let (grant, resume_token) = if let Some(retained) = retained_credential {
            retained
        } else {
            let grant = self.inner.enrollment.enroll(
                &hello.enrollment_proof,
                expected_scope.clone(),
                now_unix_ms(),
            )?;
            if grant.scope != expected_scope || grant.expires_at_unix_ms <= now_unix_ms() {
                return Err(DispatchError::input(
                    "ENROLLMENT_GRANT_INVALID",
                    "enrollment provider returned a mismatched or expired Runtime grant",
                ));
            }
            let resume_token = Uuid::new_v4().to_string();
            let pending = PendingRuntimeEnrollment {
                proof_digest,
                grant: grant.clone(),
                resume_token: resume_token.clone(),
            };
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            if registry.runtime_resume_tokens.contains_key(&runtime_key)
                || registry
                    .pending_runtime_enrollments
                    .contains_key(&runtime_key)
            {
                return Err(DispatchError::input(
                    "RESUME_TOKEN_REQUIRED",
                    "another session already enrolled this Runtime generation",
                ));
            }
            // Retain the same token/grant before attempting Welcome delivery. A
            // retry with the same one-shot proof is therefore idempotent until
            // the first authenticated post-Welcome frame confirms durable commit.
            // Welcome 投递前保留同一 token/grant；首个认证后续帧确认落盘前，
            // 相同一次性 proof 的重试保持幂等。
            registry
                .pending_runtime_enrollments
                .insert(runtime_key.clone(), pending);
            registry
                .runtime_resume_tokens
                .insert(runtime_key.clone(), resume_token.clone());
            registry
                .runtime_grants
                .insert(runtime_key.clone(), grant.clone());
            (grant, resume_token)
        };
        if grant.scope != expected_scope || grant.expires_at_unix_ms <= now_unix_ms() {
            return Err(DispatchError::input(
                "ENROLLMENT_GRANT_INVALID",
                "enrollment provider returned a mismatched or expired Runtime grant",
            ));
        }
        let session_id = Uuid::new_v4().to_string();
        let handle = Arc::new(SessionHandle::new(session_id.clone(), sender));
        let evicted = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            registry.validate_runtime_attachment(&runtime, &key)?;
            if hello.resume_token.is_empty() {
                let pending_matches = registry
                    .pending_runtime_enrollments
                    .get(&runtime_key)
                    .is_some_and(|pending| {
                        pending.proof_digest == proof_digest
                            && pending.grant == grant
                            && pending.resume_token == resume_token
                    });
                if !pending_matches {
                    return Err(DispatchError::input(
                        "RESUME_TOKEN_REQUIRED",
                        "Runtime bootstrap is no longer pending for this proof",
                    ));
                }
            } else {
                if registry.runtime_resume_tokens.get(&runtime_key) != Some(&resume_token) {
                    return Err(DispatchError::input(
                        "RESUME_TOKEN_REJECTED",
                        "Runtime Agent resume token is invalid for this Runtime and NodeKey",
                    ));
                }
            }
            registry
                .runtime_node_bindings
                .entry(runtime.clone())
                .or_insert_with(|| key.clone());
            let session = RuntimeSession {
                node: key.clone(),
                organization_id: scope.organization_id.clone(),
                workspace_id: scope.workspace_id.clone(),
                workload_identity: grant.workload_identity.clone(),
                workload_identity_expires_at_unix_ms: grant.expires_at_unix_ms,
                handle: handle.clone(),
            };
            let evicted = Vec::new();
            registry
                .runtime_resume_tokens
                .insert(runtime_key.clone(), resume_token.clone());
            registry
                .runtime_grants
                .insert(runtime_key.clone(), grant.clone());
            let replaced = registry.runtimes.insert(runtime_key.clone(), session);
            if let Some(replaced) = replaced {
                let mut evicted = evicted;
                evicted.push(replaced.handle);
                evicted
            } else {
                evicted
            }
        };
        self.fence_sessions(
            evicted,
            "Runtime session replaced or fenced by a newer authenticated connection",
        )
        .await;
        handle
            .send_welcome(control_plane_to_node::Body::ExecutionAgentWelcome(
                ExecutionAgentWelcome {
                    session_id: session_id.clone(),
                    selected_protocol_version: 2,
                    heartbeat_interval: Some(duration_to_proto(self.inner.heartbeat_interval)?),
                    server_time: Some(now_timestamp()),
                    resume_token,
                    acknowledged_agent_sequence: 0,
                    selected_contract: Some(contract_to_proto()),
                },
            ))
            .await?;
        Ok((
            handle,
            SessionIdentity::Runtime(runtime, NodeKey::from(node)),
        ))
    }

    async fn receive_loop(
        &self,
        peer: AuthenticatedAgent,
        identity: SessionIdentity,
        handle: Arc<SessionHandle>,
        first: NodeToControlPlane,
        mut inbound: Streaming<NodeToControlPlane>,
    ) {
        let mut cursor = ObservationCursor::default();
        if let Err(error) = cursor.admit(&first.frame_id, first.sequence_number) {
            handle.close_with(status_from_dispatch(error.into())).await;
            self.remove_session(&identity, &handle.session_id).await;
            return;
        }
        while let Ok(Some(frame)) = inbound.message().await {
            let result = self
                .receive_frame(&peer, &identity, &handle, &mut cursor, frame)
                .await;
            if let Err(error) = result {
                handle.close_with(status_from_dispatch(error)).await;
                break;
            }
        }
        self.remove_session(&identity, &handle.session_id).await;
    }

    async fn receive_frame(
        &self,
        peer: &AuthenticatedAgent,
        identity: &SessionIdentity,
        handle: &SessionHandle,
        cursor: &mut ObservationCursor,
        frame: NodeToControlPlane,
    ) -> Result<(), DispatchError> {
        if frame.session_id != handle.session_id || !self.is_current(identity, &handle.session_id) {
            return Err(DispatchError::input(
                "SESSION_FENCED",
                "Agent frame belongs to a stale or different session",
            ));
        }
        if cursor.admit(&frame.frame_id, frame.sequence_number)? == AdmissionDisposition::Duplicate
        {
            return Ok(());
        }
        handle.acknowledge_agent_sequence(frame.sequence_number);
        validate_frame_identity(identity, &frame)?;
        self.confirm_runtime_enrollment(identity, &handle.session_id);
        match frame.body.as_ref() {
            Some(node_to_control_plane::Body::CommandResult(result)) => {
                self.complete_command_result(identity, &handle.session_id, result)?;
            }
            Some(node_to_control_plane::Body::AssignmentAck(ack)) => {
                self.complete_assignment_ack(identity, &handle.session_id, ack)?;
            }
            Some(node_to_control_plane::Body::LeaseRenewal(request)) => {
                let SessionIdentity::Runtime(runtime, node) = identity else {
                    return Err(DispatchError::input(
                        "RUNTIME_FRAME_INVALID",
                        "Host session cannot request a Runtime Lease renewal",
                    ));
                };
                let outcome = match self.renew_lease(&node.to_proto(), runtime, request).await {
                    Ok(lease) => lease_renewal_result::Outcome::Lease(lease),
                    Err(error) => {
                        lease_renewal_result::Outcome::Rejection(semantic_v1::Rejection {
                            reason_code: error.reason_code,
                            message: error.message,
                        })
                    }
                };
                handle
                    .send(control_plane_to_node::Body::LeaseRenewalResult(
                        LeaseRenewalResult {
                            request_id: request.request_id.clone(),
                            runtime: request.runtime.clone(),
                            outcome: Some(outcome),
                        },
                    ))
                    .await?;
            }
            _ => {}
        }
        let _ = self.inner.observations.send(ControlObservation {
            agent: peer.clone(),
            session_id: handle.session_id.clone(),
            frame,
        });
        Ok(())
    }

    fn confirm_runtime_enrollment(&self, identity: &SessionIdentity, session_id: &str) {
        let SessionIdentity::Runtime(runtime, node) = identity else {
            return;
        };
        let key = RuntimeKey {
            runtime: runtime.clone(),
            node: node.clone(),
        };
        let mut registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        let current = registry.runtimes.get(&key).is_some_and(|session| {
            session.handle.session_id == session_id && session.handle.is_usable()
        });
        if current {
            registry.pending_runtime_enrollments.remove(&key);
        }
    }

    fn is_current(&self, identity: &SessionIdentity, session_id: &str) -> bool {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("execution session registry lock poisoned");
        match identity {
            SessionIdentity::Host(node) => registry.hosts.get(node).is_some_and(|session| {
                registry.highest_node_epochs.get(&node.node_id) == Some(&node.node_epoch)
                    && session.handle.session_id == session_id
                    && session.handle.is_usable()
            }),
            SessionIdentity::Runtime(runtime, node) => registry
                .runtimes
                .get(&RuntimeKey {
                    runtime: runtime.clone(),
                    node: node.clone(),
                })
                .is_some_and(|session| {
                    registry.highest_node_epochs.get(&node.node_id) == Some(&node.node_epoch)
                        && session.handle.session_id == session_id
                        && session.handle.is_usable()
                }),
        }
    }

    async fn remove_session(&self, identity: &SessionIdentity, session_id: &str) {
        let removed = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("execution session registry lock poisoned");
            match identity {
                SessionIdentity::Host(node)
                    if registry
                        .hosts
                        .get(node)
                        .is_some_and(|session| session.handle.session_id == session_id) =>
                {
                    registry.hosts.remove(node).map(|session| session.handle)
                }
                SessionIdentity::Runtime(runtime, node)
                    if registry
                        .runtimes
                        .get(&RuntimeKey {
                            runtime: runtime.clone(),
                            node: node.clone(),
                        })
                        .is_some_and(|session| session.handle.session_id == session_id) =>
                {
                    registry
                        .runtimes
                        .remove(&RuntimeKey {
                            runtime: runtime.clone(),
                            node: node.clone(),
                        })
                        .map(|session| session.handle)
                }
                _ => None,
            }
        };
        if let Some(handle) = removed {
            handle.fence().await;
            let mut session_ids = BTreeSet::new();
            session_ids.insert(handle.session_id.clone());
            self.remove_pending_for_sessions(&session_ids);
        }
    }
}

#[derive(Debug, Clone)]
enum SessionIdentity {
    Host(NodeKey),
    Runtime(semantic::Identity, NodeKey),
}

#[tonic::async_trait]
impl NodeControlService for ExecutionControlService {
    type ConnectStream = ResponseStream;

    async fn connect(
        &self,
        request: Request<Streaming<NodeToControlPlane>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let certificates = request
            .peer_certs()
            .map(|certificates| {
                certificates
                    .iter()
                    .map(|certificate| certificate.get_ref().to_vec())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let peer = self
            .inner
            .authenticator
            .authenticate(&certificates, request.metadata())
            .map_err(status_from_dispatch)?;
        let mut inbound = request.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("Agent stream closed before Hello"))?;
        let (sender, receiver) = mpsc::channel(64);
        let (handle, identity) = self
            .establish(peer.clone(), first.clone(), sender)
            .await
            .map_err(status_from_dispatch)?;
        let service = self.clone();
        tokio::spawn(async move {
            service
                .receive_loop(peer, identity, handle, first, inbound)
                .await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

fn validate_frame_identity(
    identity: &SessionIdentity,
    frame: &NodeToControlPlane,
) -> Result<(), DispatchError> {
    match identity {
        SessionIdentity::Host(node) => match frame.body.as_ref() {
            Some(node_to_control_plane::Body::Heartbeat(heartbeat)) => {
                let received = heartbeat.node.as_ref().ok_or_else(|| {
                    DispatchError::input(
                        "NODE_IDENTITY_REQUIRED",
                        "Host heartbeat requires NodeRef",
                    )
                })?;
                if NodeKey::from(received) != *node {
                    return Err(DispatchError::input(
                        "AUTHENTICATED_NODE_MISMATCH",
                        "Host heartbeat changed the authenticated NodeRef",
                    ));
                }
            }
            Some(node_to_control_plane::Body::CommandResult(result))
                if !result.command_id.is_empty() => {}
            Some(node_to_control_plane::Body::OperationEvent(_)) => {}
            _ => {
                return Err(DispatchError::input(
                    "HOST_FRAME_INVALID",
                    "Host session sent a frame outside the Host Agent protocol",
                ))
            }
        },
        SessionIdentity::Runtime(runtime, node) => {
            if let Some(node_to_control_plane::Body::Heartbeat(heartbeat)) = frame.body.as_ref() {
                let received = heartbeat.node.as_ref().ok_or_else(|| {
                    DispatchError::input(
                        "NODE_IDENTITY_REQUIRED",
                        "Runtime heartbeat requires NodeRef",
                    )
                })?;
                if NodeKey::from(received) != *node {
                    return Err(DispatchError::input(
                        "AUTHENTICATED_NODE_MISMATCH",
                        "Runtime heartbeat changed the authenticated NodeRef",
                    ));
                }
                return Ok(());
            }
            let received = runtime_for_frame(frame).ok_or_else(|| {
                DispatchError::input(
                    "RUNTIME_FRAME_INVALID",
                    "Runtime session sent a frame without its Runtime identity",
                )
            })?;
            if &received != runtime {
                return Err(DispatchError::input(
                    "AUTHENTICATED_RUNTIME_MISMATCH",
                    "Runtime frame changed the authenticated Runtime generation",
                ));
            }
        }
    }
    Ok(())
}

fn validate_user_and_actions(
    user_id: &str,
    allowed_actions: &[String],
) -> Result<(), DispatchError> {
    if user_id.trim().is_empty() {
        return Err(DispatchError::input(
            "WORKLOAD_USER_REQUIRED",
            "workload identity requires a non-empty user id",
        ));
    }
    if user_id.len() > MAX_WORKLOAD_USER_BYTES {
        return Err(DispatchError::input(
            "WORKLOAD_USER_LIMIT",
            format!("workload identity user id allows at most {MAX_WORKLOAD_USER_BYTES} bytes"),
        ));
    }
    if allowed_actions.is_empty() {
        return Err(DispatchError::input(
            "WORKLOAD_ACTIONS_INVALID",
            "workload identity requires at least one action",
        ));
    }
    if allowed_actions.len() > MAX_WORKLOAD_ACTIONS {
        return Err(DispatchError::input(
            "WORKLOAD_ACTIONS_LIMIT",
            format!("workload identity allows at most {MAX_WORKLOAD_ACTIONS} actions"),
        ));
    }
    let mut unique = std::collections::BTreeSet::new();
    for action in allowed_actions {
        if action.trim().is_empty() {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_INVALID",
                "workload identity actions cannot be empty",
            ));
        }
        if action.len() > MAX_WORKLOAD_ACTION_BYTES {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_LIMIT",
                format!(
                    "workload identity actions allow at most {MAX_WORKLOAD_ACTION_BYTES} bytes"
                ),
            ));
        }
        if !unique.insert(action) {
            return Err(DispatchError::input(
                "WORKLOAD_ACTION_DUPLICATE",
                "workload identity actions must be unique",
            ));
        }
    }
    Ok(())
}

fn validate_workload_identity_payload(
    workload: Option<&core_v1::WorkloadIdentity>,
) -> Result<(), DispatchError> {
    let workload = workload.ok_or_else(|| {
        DispatchError::input(
            "WORKLOAD_IDENTITY_REQUIRED",
            "Runtime assignment requires a workload identity",
        )
    })?;
    let scope = workload.scope.as_ref().ok_or_else(|| {
        DispatchError::input(
            "WORKLOAD_SCOPE_REQUIRED",
            "Runtime assignment workload identity requires an account scope",
        )
    })?;
    validate_user_and_actions(&scope.user_id, &workload.allowed_actions)?;
    Ok(())
}

fn semantic_identity_from_proto(
    identity: &semantic_v1::Identity,
) -> Result<semantic::Identity, DispatchError> {
    let identity = semantic::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    };
    identity.validate().map_err(|error| {
        DispatchError::input(
            error.reason_code,
            format!("invalid semantic identity: {}", error.message),
        )
    })?;
    Ok(identity)
}

fn runtime_for_frame(frame: &NodeToControlPlane) -> Option<semantic::Identity> {
    let runtime = match frame.body.as_ref()? {
        node_to_control_plane::Body::AssignmentAck(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::ExecutionInventory(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeHeartbeat(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::LeaseRenewal(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeObservation(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::RuntimeProgress(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::StructuredEvent(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::LogReference(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::StopAck(value) => value.runtime.as_ref(),
        node_to_control_plane::Body::Heartbeat(_) => return None,
        _ => return None,
    }?;
    runtime
        .identity
        .as_ref()
        .map(|identity| semantic::Identity {
            id: identity.id.clone(),
            generation: identity.generation,
        })
}

fn runtime_identity(hello: &ExecutionAgentHello) -> Result<semantic::Identity, DispatchError> {
    let identity = hello
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.identity.as_ref())
        .ok_or_else(|| {
            DispatchError::input("RUNTIME_IDENTITY_REQUIRED", "Runtime identity is required")
        })?;
    Ok(semantic::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    })
}

fn authority_lease_result(
    result: KernelCommandResult,
) -> Result<semantic_v1::Lease, DispatchError> {
    match result.outcome {
        Some(kernel_command_result::Outcome::Authority(authority)) => match authority.outcome {
            Some(kernel_authority_command_result::Outcome::Lease(lease)) => Ok(lease),
            _ => Err(DispatchError::unknown(
                "Kernel authority command returned an unexpected typed outcome",
            )),
        },
        Some(kernel_command_result::Outcome::Error(status)) => Err(kernel_status_error(status)),
        _ => Err(DispatchError::unknown(
            "Node Agent returned an unexpected Kernel command outcome",
        )),
    }
}

fn kernel_status_error(status: cy_proto::google::rpc::Status) -> DispatchError {
    let structured = status.details.iter().find_map(|detail| {
        (detail.type_url == "type.googleapis.com/cyrene.semantic.v1.Rejection")
            .then(|| semantic_v1::Rejection::decode(detail.value.as_slice()).ok())
            .flatten()
    });
    if let Some(rejection) = structured {
        return DispatchError::authority(semantic::Rejection::new(
            rejection.reason_code,
            rejection.message,
        ));
    }
    let code = Code::from_i32(status.code);
    if matches!(
        code,
        Code::Ok
            | Code::Cancelled
            | Code::Unknown
            | Code::DeadlineExceeded
            | Code::Aborted
            | Code::Internal
            | Code::Unavailable
            | Code::DataLoss
    ) {
        return DispatchError::unknown(format!(
            "Kernel authority transport returned {code}: {}",
            status.message
        ));
    }
    DispatchError::authority(semantic::Rejection::new(
        "KERNEL_AUTHORITY_REJECTED",
        status.message,
    ))
}

fn context_to_proto(context: &AuthorityCallContext) -> core_v1::AuthorityCallContext {
    core_v1::AuthorityCallContext {
        contract: Some(semantic_v1::ContractRevision {
            contract_id: context.contract.contract_id.clone(),
            major: context.contract.major,
            minor: context.contract.minor,
        }),
        request_id: context.request_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
    }
}

fn identity_to_proto(identity: &semantic::Identity) -> semantic_v1::Identity {
    semantic_v1::Identity {
        id: identity.id.clone(),
        generation: identity.generation,
    }
}

fn resource_query_to_proto(query: &semantic::ResourceQuery) -> semantic_v1::ResourceQuery {
    semantic_v1::ResourceQuery {
        resource_class: query.resource_class.clone(),
        count: query.count,
        required_capabilities: query
            .required_capabilities
            .iter()
            .map(|requirement| semantic_v1::CapabilityRequirement {
                id: requirement.id.clone(),
                minimum_revision: requirement.minimum_revision,
                required_properties: requirement
                    .required_properties
                    .clone()
                    .into_iter()
                    .collect(),
            })
            .collect(),
        minimum_capacity: query
            .minimum_capacity
            .iter()
            .map(|(key, quantity)| {
                (
                    key.clone(),
                    semantic_v1::Quantity {
                        value: quantity.value,
                        unit: quantity.unit.clone(),
                    },
                )
            })
            .collect(),
    }
}

fn duration_to_proto(duration: Duration) -> Result<prost_types::Duration, DispatchError> {
    let seconds = i64::try_from(duration.as_secs()).map_err(|_| {
        DispatchError::input("DURATION_INVALID", "duration seconds exceed Protobuf range")
    })?;
    let nanos = i32::try_from(duration.subsec_nanos()).map_err(|_| {
        DispatchError::input("DURATION_INVALID", "duration nanos exceed Protobuf range")
    })?;
    Ok(prost_types::Duration { seconds, nanos })
}

fn contract_to_proto() -> semantic_v1::ContractRevision {
    let revision = semantic::ContractRevision::current();
    semantic_v1::ContractRevision {
        contract_id: revision.contract_id,
        major: revision.major,
        minor: revision.minor,
    }
}

fn supported_contract(revision: &semantic_v1::ContractRevision) -> bool {
    revision == &contract_to_proto()
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn timestamp_unix_ms(timestamp: &prost_types::Timestamp) -> Option<u64> {
    if timestamp.seconds <= 0 || timestamp.nanos < 0 || timestamp.nanos % 1_000_000 != 0 {
        return None;
    }
    u64::try_from(timestamp.seconds)
        .ok()?
        .checked_mul(1000)?
        .checked_add(u64::try_from(timestamp.nanos / 1_000_000).ok()?)
}

fn now_timestamp() -> prost_types::Timestamp {
    timestamp_from_unix_ms(now_unix_ms())
}

fn timestamp_from_unix_ms(unix_ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(unix_ms / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((unix_ms % 1000) * 1_000_000).unwrap_or_default(),
    }
}

fn status_from_dispatch(error: DispatchError) -> Status {
    let message = format!("{}: {}", error.reason_code, error.message);
    if error.reconciliation_required {
        Status::aborted(message)
    } else if error.reason_code.contains("AUTHENTICATED")
        || error.reason_code.contains("CERTIFICATE")
        || error.reason_code.contains("RESUME_TOKEN")
    {
        Status::unauthenticated(message)
    } else {
        Status::failed_precondition(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(epoch: u64) -> NodeKey {
        NodeKey {
            node_id: "node-test".to_string(),
            node_epoch: epoch,
        }
    }

    fn runtime() -> semantic::Identity {
        semantic::Identity {
            id: "runtime-test".to_string(),
            generation: 1,
        }
    }

    #[tokio::test]
    async fn higher_node_epoch_evicts_lower_sessions_and_bindings() {
        let old = node(1);
        let new = node(2);
        let (sender, _receiver) = mpsc::channel(1);
        let handle = Arc::new(SessionHandle::new("old-session".to_string(), sender));
        let mut registry = Registry::default();
        registry.admit_node_epoch(&old).unwrap();
        registry.hosts.insert(
            old.clone(),
            HostSession {
                handle: handle.clone(),
            },
        );
        registry.accepted_assignments.insert(
            "assignment-test".to_string(),
            AssignmentBinding {
                assignment_id: "assignment-test".to_string(),
                runtime: runtime(),
                node: old.clone(),
                lease: LeaseKey {
                    identity: semantic::Identity {
                        id: "lease-test".to_string(),
                        generation: 1,
                    },
                    fence_token: 1,
                },
                digest: [0; 32],
            },
        );

        let evicted = registry.admit_node_epoch(&new).unwrap();

        assert_eq!(evicted.len(), 1);
        assert!(registry.hosts.is_empty());
        assert!(registry.accepted_assignments.is_empty());
        assert_eq!(registry.highest_node_epochs.get("node-test"), Some(&2));
        evicted[0].fence().await;
        assert!(handle.is_fenced());
    }

    #[test]
    fn lower_node_epoch_is_rejected_without_mutation() {
        let mut registry = Registry::default();
        registry.admit_node_epoch(&node(3)).unwrap();

        let error = match registry.admit_node_epoch(&node(2)) {
            Ok(_) => panic!("a lower Node epoch must be rejected"),
            Err(error) => error,
        };

        assert_eq!(error.reason_code, "STALE_NODE_GENERATION");
        assert_eq!(registry.highest_node_epochs.get("node-test"), Some(&3));
    }

    #[tokio::test]
    async fn runtime_resume_token_stays_stable_across_reconnect() {
        let service = ExecutionControlService::new(
            Arc::new(
                crate::authentication::CertificateFingerprintAuthenticator::new([(
                    b"test-certificate".to_vec(),
                    AuthenticatedAgent::Runtime {
                        runtime: runtime(),
                        node: node(1).to_proto(),
                    },
                )])
                .unwrap(),
            ),
            Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
                ["unused-proof".to_string()],
                60_000,
            )),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let runtime = runtime();
        let node = node(1);
        let runtime_key = RuntimeKey {
            runtime: runtime.clone(),
            node: node.clone(),
        };
        let (host_sender, _host_receiver) = mpsc::channel(1);
        {
            let mut registry = service.inner.registry.lock().unwrap();
            registry.highest_node_epochs.insert(node.node_id.clone(), 1);
            let host_handle = Arc::new(SessionHandle::new("host-session".to_string(), host_sender));
            host_handle.mark_ready_for_test();
            registry.hosts.insert(
                node.clone(),
                HostSession {
                    handle: host_handle,
                },
            );
            registry
                .runtime_node_bindings
                .insert(runtime.clone(), node.clone());
            registry
                .runtime_resume_tokens
                .insert(runtime_key.clone(), "resume-old".to_string());
            registry.runtime_grants.insert(
                runtime_key.clone(),
                EnrollmentGrant {
                    workload_identity: semantic::Identity {
                        id: "workload-test".to_string(),
                        generation: 1,
                    },
                    scope: RuntimeScope {
                        organization_id: "organization-test".to_string(),
                        workspace_id: "workspace-test".to_string(),
                        runtime: runtime.clone(),
                    },
                    expires_at_unix_ms: now_unix_ms() + 60_000,
                },
            );
        }
        let hello = runtime_resume_hello(&runtime, &node, "resume-old");
        let (first_sender, _first_receiver) = mpsc::channel(4);
        let (first_handle, _) = service
            .establish_runtime(runtime.clone(), node.to_proto(), &hello, first_sender)
            .await
            .unwrap();

        let (replacement_sender, _replacement_receiver) = mpsc::channel(4);
        service
            .establish_runtime(runtime, node.to_proto(), &hello, replacement_sender)
            .await
            .unwrap();

        assert!(first_handle.is_fenced());
        let registry = service.inner.registry.lock().unwrap();
        assert_eq!(
            registry
                .runtime_resume_tokens
                .get(&runtime_key)
                .map(String::as_str),
            Some("resume-old")
        );
        assert_eq!(registry.runtimes.len(), 1);
    }

    #[tokio::test]
    async fn pending_bootstrap_replays_same_token_until_first_agent_frame() {
        let service = ExecutionControlService::new(
            Arc::new(
                crate::authentication::CertificateFingerprintAuthenticator::new([(
                    b"test-certificate".to_vec(),
                    AuthenticatedAgent::Runtime {
                        runtime: runtime(),
                        node: node(1).to_proto(),
                    },
                )])
                .unwrap(),
            ),
            Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
                ["one-shot-proof".to_string()],
                60_000,
            )),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let runtime = runtime();
        let node = node(1);
        let runtime_key = RuntimeKey {
            runtime: runtime.clone(),
            node: node.clone(),
        };
        let (host_sender, _host_receiver) = mpsc::channel(1);
        {
            let mut registry = service.inner.registry.lock().unwrap();
            registry.highest_node_epochs.insert(node.node_id.clone(), 1);
            let host_handle = Arc::new(SessionHandle::new("host-session".to_string(), host_sender));
            host_handle.mark_ready_for_test();
            registry.hosts.insert(
                node.clone(),
                HostSession {
                    handle: host_handle,
                },
            );
        }
        let mut hello = runtime_resume_hello(&runtime, &node, "");
        hello.enrollment_proof = "one-shot-proof".to_string();

        let (first_sender, mut first_receiver) = mpsc::channel(4);
        service
            .establish_runtime(runtime.clone(), node.to_proto(), &hello, first_sender)
            .await
            .unwrap();
        let first_token = runtime_welcome_token(first_receiver.recv().await.unwrap().unwrap());

        let (retry_sender, mut retry_receiver) = mpsc::channel(4);
        let (retry_handle, retry_identity) = service
            .establish_runtime(runtime.clone(), node.to_proto(), &hello, retry_sender)
            .await
            .unwrap();
        let retry_token = runtime_welcome_token(retry_receiver.recv().await.unwrap().unwrap());
        assert_eq!(first_token, retry_token);
        assert!(service
            .inner
            .registry
            .lock()
            .unwrap()
            .pending_runtime_enrollments
            .contains_key(&runtime_key));

        service.confirm_runtime_enrollment(&retry_identity, &retry_handle.session_id);
        assert!(!service
            .inner
            .registry
            .lock()
            .unwrap()
            .pending_runtime_enrollments
            .contains_key(&runtime_key));

        let (late_sender, _late_receiver) = mpsc::channel(4);
        let error = match service
            .establish_runtime(runtime, node.to_proto(), &hello, late_sender)
            .await
        {
            Ok(_) => panic!("confirmed bootstrap proof must not be replayed"),
            Err(error) => error,
        };
        assert_eq!(error.reason_code, "RESUME_TOKEN_REQUIRED");
    }

    #[test]
    fn unavailable_kernel_result_requires_reconciliation() {
        let result = KernelCommandResult {
            command_id: "command-1".to_string(),
            outcome: Some(kernel_command_result::Outcome::Error(
                cy_proto::google::rpc::Status {
                    code: Code::Unavailable as i32,
                    message: "UDS response was lost".to_string(),
                    details: Vec::new(),
                },
            )),
        };

        let error = authority_lease_result(result).unwrap_err();
        assert_eq!(error.reason_code, "UNKNOWN_REQUIRES_RECONCILIATION");
        assert!(error.reconciliation_required);
    }

    #[test]
    fn structured_kernel_rejection_remains_deterministic() {
        let result = KernelCommandResult {
            command_id: "command-1".to_string(),
            outcome: Some(kernel_command_result::Outcome::Error(
                cy_proto::google::rpc::Status {
                    code: Code::FailedPrecondition as i32,
                    message: "lease rejected".to_string(),
                    details: vec![prost_types::Any {
                        type_url: "type.googleapis.com/cyrene.semantic.v1.Rejection".to_string(),
                        value: semantic_v1::Rejection {
                            reason_code: "LEASE_FENCED".to_string(),
                            message: "lease fence is stale".to_string(),
                        }
                        .encode_to_vec(),
                    }],
                },
            )),
        };

        let error = authority_lease_result(result).unwrap_err();
        assert_eq!(error.reason_code, "LEASE_FENCED");
        assert!(!error.reconciliation_required);
    }

    #[test]
    fn workload_user_and_actions_are_bounded_and_unique() {
        assert_eq!(
            validate_user_and_actions("", &["operation.run".to_string()])
                .unwrap_err()
                .reason_code,
            "WORKLOAD_USER_REQUIRED"
        );
        assert_eq!(
            validate_user_and_actions(
                &"界".repeat(MAX_WORKLOAD_USER_BYTES / "界".len() + 1),
                &["operation.run".to_string()]
            )
            .unwrap_err()
            .reason_code,
            "WORKLOAD_USER_LIMIT"
        );
        assert_eq!(
            validate_user_and_actions(
                "user",
                &["界".repeat(MAX_WORKLOAD_ACTION_BYTES / "界".len() + 1)]
            )
            .unwrap_err()
            .reason_code,
            "WORKLOAD_ACTION_LIMIT"
        );
        assert_eq!(
            validate_user_and_actions(
                "user",
                &["operation.run".to_string(), "operation.run".to_string()]
            )
            .unwrap_err()
            .reason_code,
            "WORKLOAD_ACTION_DUPLICATE"
        );
        assert_eq!(
            validate_user_and_actions("user", &vec!["a".to_string(); MAX_WORKLOAD_ACTIONS + 1])
                .unwrap_err()
                .reason_code,
            "WORKLOAD_ACTIONS_LIMIT"
        );
    }

    #[test]
    fn pending_binding_includes_session_and_node_identity() {
        let first = SessionBinding::for_identity(&SessionIdentity::Host(node(1)), "session-one");
        let second = SessionBinding::for_identity(&SessionIdentity::Host(node(1)), "session-two");
        let different_node =
            SessionBinding::for_identity(&SessionIdentity::Host(node(2)), "session-one");

        assert_ne!(first, second);
        assert_ne!(first, different_node);
    }

    fn runtime_resume_hello(
        runtime: &semantic::Identity,
        node: &NodeKey,
        resume_token: &str,
    ) -> ExecutionAgentHello {
        ExecutionAgentHello {
            runtime: Some(core_v1::RuntimeRef {
                identity: Some(identity_to_proto(runtime)),
            }),
            scope: Some(core_v1::AccountScope {
                user_id: String::new(),
                organization_id: "organization-test".to_string(),
                workspace_id: "workspace-test".to_string(),
            }),
            attachment_type: core_v1::ExecutionAttachmentType::ContainerAgent as i32,
            persistence_class: core_v1::PersistenceClass::Ephemeral as i32,
            agent_version: "test".to_string(),
            min_protocol_version: 2,
            max_protocol_version: 2,
            resume_token: resume_token.to_string(),
            capabilities: vec![cy_execution_fabric::execution_capability(
                core_v1::ExecutionAttachmentType::ContainerAgent,
                false,
                core_v1::RestartCapability::None,
            )],
            enrollment_proof: String::new(),
            node: Some(core_v1::ExecutionNodeDescriptor {
                node: Some(node.to_proto()),
                node_type: "container".to_string(),
                persistent: Some(false),
            }),
            restart_capability: core_v1::RestartCapability::None as i32,
        }
    }

    fn runtime_welcome_token(frame: cy_proto::core_v1::ControlPlaneToNode) -> String {
        match frame.body {
            Some(control_plane_to_node::Body::ExecutionAgentWelcome(welcome)) => {
                welcome.resume_token
            }
            _ => panic!("expected Runtime Welcome"),
        }
    }

    #[test]
    fn mismatched_command_result_does_not_consume_waiter() {
        let service = ExecutionControlService::new(
            Arc::new(
                crate::authentication::CertificateFingerprintAuthenticator::new([(
                    b"test-certificate".to_vec(),
                    AuthenticatedAgent::Host {
                        node: node(1).to_proto(),
                    },
                )])
                .unwrap(),
            ),
            Arc::new(cy_execution_fabric::DevelopmentEnrollmentProvider::new(
                ["unused-proof".to_string()],
                60_000,
            )),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let (sender, mut receiver) = oneshot::channel();
        service.inner.pending_commands.lock().unwrap().insert(
            "command-test".to_string(),
            PendingCommand {
                binding: SessionBinding::Host {
                    node: node(1),
                    session_id: "session-one".to_string(),
                },
                sender,
            },
        );

        let error = service
            .complete_command_result(
                &SessionIdentity::Host(node(2)),
                "session-two",
                &KernelCommandResult {
                    command_id: "command-test".to_string(),
                    ..Default::default()
                },
            )
            .unwrap_err();

        assert_eq!(error.reason_code, "PENDING_COMMAND_SESSION_MISMATCH");
        assert!(service
            .inner
            .pending_commands
            .lock()
            .unwrap()
            .contains_key("command-test"));
        assert!(receiver.try_recv().is_err());
    }
}
