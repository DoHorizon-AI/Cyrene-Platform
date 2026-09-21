//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 events.rs                                                       │
//! │  Module: cy_observability::events                                   │
//! │  Role: Canonical platform event names and metadata constants.        │
//! │                                                                     │
//! │  模块职责：Platform 第一批稳定事件名常量与元数据定义。                    │
//! └─────────────────────────────────────────────────────────────────────┘

/// Service started event.
pub const EVENT_SERVICE_STARTED: &str = "platform.service.started";
/// Service stopped event.
pub const EVENT_SERVICE_STOPPED: &str = "platform.service.stopped";

/// Lease acquired event.
pub const EVENT_LEASE_ACQUIRED: &str = "platform.lease.acquired";
/// Lease released event.
pub const EVENT_LEASE_RELEASED: &str = "platform.lease.released";
/// Lease release deferred/failed event (fail-closed reservation maintained).
pub const EVENT_LEASE_RELEASE_DEFERRED: &str = "platform.lease.release_deferred";

/// Worker started event.
pub const EVENT_WORKER_STARTED: &str = "platform.worker.started";
/// Worker terminated event.
pub const EVENT_WORKER_TERMINATED: &str = "platform.worker.terminated";
/// Worker reaped by watchdog event.
pub const EVENT_WORKER_REAPED: &str = "platform.worker.reaped";
/// Worker heartbeat lost event.
pub const EVENT_WORKER_HEARTBEAT_LOST: &str = "platform.worker.heartbeat_lost";

/// Node connected to control plane event.
pub const EVENT_NODE_CONNECTED: &str = "platform.node.connected";
/// Node disconnected from control plane event.
pub const EVENT_NODE_DISCONNECTED: &str = "platform.node.disconnected";
/// Node reconnecting attempt summary event.
pub const EVENT_NODE_RECONNECTING: &str = "platform.node.reconnecting";
/// Node reconnected successfully event.
pub const EVENT_NODE_RECONNECTED: &str = "platform.node.reconnected";

/// Sandbox cgroup initialized.
pub const EVENT_SANDBOX_CGROUP_CREATED: &str = "platform.sandbox.cgroup_created";
/// Sandbox process terminated.
pub const EVENT_SANDBOX_TERMINATED: &str = "platform.sandbox.terminated";

/// Package activated.
pub const EVENT_PACKAGE_ACTIVATED: &str = "platform.package.activated";
/// Package deactivated.
pub const EVENT_PACKAGE_DEACTIVATED: &str = "platform.package.deactivated";

/// Relay connection established.
pub const EVENT_RELAY_CONNECTED: &str = "platform.relay.connected";
/// Relay connection closed or lost.
pub const EVENT_RELAY_DISCONNECTED: &str = "platform.relay.disconnected";

/// Reconciliation evaluation decision.
pub const EVENT_RECONCILE_EVALUATED: &str = "platform.reconcile.evaluated";

/// Unhandled panic diagnostic event.
pub const EVENT_PANIC: &str = "platform.panic";
