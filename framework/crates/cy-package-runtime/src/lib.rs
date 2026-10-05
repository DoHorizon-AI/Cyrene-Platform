//! Internal Platform service for immutable capability package lifecycle.
//!
//! Package descriptors and repository manifests remain owned by Workspace and
//! plugin repositories. This crate consumes those authorities, publishes verified
//! installations atomically, prepares locked dependencies, and delegates
//! Plugin process supervision to a Product-neutral service supervisor. The
//! public package/control structs here are host implementation formats, not a
//! supported arbitrary-license extension SDK; external integrations cross the
//! documented process protocol.

mod bootstrap;
mod control;
mod dependency;
mod descriptor;
mod local_control;
mod runtime;
mod supervisor;
mod types;

pub use bootstrap::{
    BootstrapCandidateInput, BootstrapInstallInput, BootstrapInstallReceipt,
    BootstrapMaintenanceInput, BootstrapWorkerError, BootstrapWorkerInput, BootstrapWorkerOutput,
    ValidatedMaintenanceHold, cleanup_worker_candidate_handoff, ensure_runtime_daemon_stopped,
    ensure_runtime_state_root, parse_bootstrap_input, prepare_worker_candidate,
    read_bootstrap_file, read_bootstrap_stdin, read_operator_token,
    validate_bootstrap_input_file_location, validate_bootstrap_toolchain, validate_candidate_paths,
    validate_maintenance_hold, validate_persistent_candidate_paths, verify_root_worker_peer,
};
pub use control::{ControlCommand, ControlRequest, ControlResponse, PackageRuntimeControlServer};
pub use dependency::{CommandDependencyPreparer, DependencyPreparer};
pub use local_control::PackageRuntimeSocketServer;
pub use runtime::{FilesystemPackageRuntime, RuntimeProcessLock};
pub use supervisor::{
    PluginServiceSupervisor, ProcessPluginServiceSupervisor, ServiceActivationOptions,
};
pub use types::*;
