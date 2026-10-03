//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Module: cy_workspace_control_plane::authority                     │
//! │  Role: Platform-owned Authority RPC and durable invocation ports.  │
//! │                                                                     │
//! │  模块职责：Platform Authority RPC 实现与持久调用端口。                │
//! └─────────────────────────────────────────────────────────────────────┘

pub mod outbox;
pub mod service;
pub mod snapshot;
pub mod trust;

pub use outbox::*;
pub use service::*;
pub use snapshot::*;
pub use trust::*;
