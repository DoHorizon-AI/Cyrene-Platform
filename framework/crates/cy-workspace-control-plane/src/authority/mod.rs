//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Module: cy_workspace_control_plane::authority                     │
//! │  Role: Platform-owned Authority RPC and durable invocation ports.  │
//! │                                                                     │
//! │  模块职责：Platform Authority RPC 实现与持久调用端口。                │
//! └─────────────────────────────────────────────────────────────────────┘

pub mod outbox;
pub mod snapshot;
pub mod trust;
pub mod service;

pub use outbox::*;
pub use snapshot::*;
pub use trust::*;
pub use service::*;
