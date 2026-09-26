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
mod docker_launcher;
mod intent;
mod launcher;
mod server;
mod session;
mod session_store;

pub use authentication::{
    AuthenticatedAgent, CertificateFingerprintAuthenticator, PeerAuthenticator,
};
pub use controller::{
    DispatchError, DispatchReceipt, ExecutionController, ExecutionDispatchRequest,
    ExecutionReleaseRequest, ExecutionStopRequest, StopDisposition, StopReceipt,
};
pub use docker_launcher::{DockerLaunchConfig, DockerResourceBinding, DockerRuntimeLauncher};
pub use intent::{
    ExecutionIntentRecord, ExecutionIntentStore, FileExecutionIntentStore,
    InMemoryExecutionIntentStore, IntentDisposition, IntentStoreError, IntentStoreErrorKind,
};
pub use launcher::RuntimeLauncher;
pub use server::{ControlObservation, ExecutionControlService};
pub use session_store::{ExecutionSessionStore, FileExecutionSessionStore};
