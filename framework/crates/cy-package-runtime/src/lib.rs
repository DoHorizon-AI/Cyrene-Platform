//! Product-neutral production lifecycle for immutable capability packages.
//!
//! Package descriptors and official manifests remain owned by Workspace and
//! Official Plugins. This crate consumes those authorities, publishes verified
//! installations atomically, prepares locked dependencies, and delegates
//! execution to the existing Platform capability worker runtime.

mod control;
mod dependency;
mod descriptor;
mod runtime;
mod supervisor;
mod types;

pub use control::{ControlCommand, ControlRequest, ControlResponse, PackageRuntimeControlServer};
pub use dependency::{DependencyPreparer, PythonVenvDependencyPreparer};
pub use runtime::FilesystemPackageRuntime;
pub use supervisor::{PlatformWorkerSupervisor, WorkerSupervisor};
pub use types::*;
