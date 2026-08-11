//! Privileged CYRENE Linux cgroup v2 Sandbox Adapter Host runtime.
//!
//! The configured root is an **owned and delegated** cgroup subtree, never the
//! cgroup mount root. Startup reaps only direct instance cgroups below that
//! subtree; this is the boundary that makes Fate Sharing cleanup safe.

#![deny(unsafe_op_in_unsafe_fn)]

pub(crate) mod bpf;
pub(crate) mod config;
mod peer;
mod protocol;
pub(crate) mod runtime;
pub(crate) mod sys;

#[cfg(test)]
mod tests;

pub use bpf::LinuxDeviceMapper;
pub use config::{CgroupV2Config, OwnedCgroupCleanupReport};
pub use peer::client_peer_credentials_allowed;
#[cfg(unix)]
pub use peer::verify_client_peer;
pub use protocol::handle_request;
pub use runtime::CgroupV2Runtime;
pub use sys::read_oom_kill_count;
