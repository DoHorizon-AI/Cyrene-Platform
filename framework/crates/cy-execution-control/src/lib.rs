//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_execution_control                                       │
//! │  Role: Route placement through canonical Lease authority.           │
//! │                                                                     │
//! │  模块职责：把 placement 经 canonical Lease authority 路由到 Runtime。 │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod authentication;
mod controller;
mod intent;
mod server;
mod session;

pub use authentication::{
    AuthenticatedAgent, CertificateFingerprintAuthenticator, PeerAuthenticator,
};
pub use controller::{
    DispatchError, DispatchReceipt, ExecutionController, ExecutionDispatchRequest,
    ExecutionReleaseRequest,
};
pub use intent::{
    ExecutionIntentRecord, ExecutionIntentStore, FileExecutionIntentStore,
    InMemoryExecutionIntentStore, IntentDisposition, IntentStoreError, IntentStoreErrorKind,
};
pub use server::{ControlObservation, ExecutionControlService};
