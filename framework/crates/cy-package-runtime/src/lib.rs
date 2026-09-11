//! Product-neutral production lifecycle for immutable capability packages.
//!
//! Package descriptors and repository manifests remain owned by Workspace and
//! plugin repositories. This crate consumes those authorities, publishes verified
//! installations atomically, prepares locked dependencies, and delegates
//! Plugin process supervision to a Product-neutral service supervisor.

mod control;
mod dependency;
mod descriptor;
mod runtime;
mod supervisor;
mod types;

pub use control::{ControlCommand, ControlRequest, ControlResponse, PackageRuntimeControlServer};
pub use dependency::{CommandDependencyPreparer, DependencyPreparer};
pub use runtime::FilesystemPackageRuntime;
pub use supervisor::{
    PluginServiceSupervisor, ProcessPluginServiceSupervisor, ServiceActivationOptions,
};
pub use types::*;
