//! CYRENE Node Agent module.
//!
//! Provides local journaling, hardware probing, gRPC AgentService handlers,
//! and self-upgrade protocol support.

pub mod journal;
pub mod probe;
pub mod service;
pub mod upgrade;

pub use journal::*;
pub use probe::*;
pub use service::*;
pub use upgrade::*;

pub const SCAFFOLD_STATUS: &str =
    "CYRENE Node Agent: Hardware Probing, Local Journaling & AgentService online";
