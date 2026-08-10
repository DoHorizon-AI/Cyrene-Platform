//! CYRENE Node Agent module.
//!
//! Provides local journaling, hardware probing, Core v1 session state, and
//! self-upgrade protocol support.

pub mod journal;
pub mod probe;
pub mod session;
pub mod upgrade;

pub use journal::*;
pub use probe::*;
pub use session::*;
pub use upgrade::*;

pub const SCAFFOLD_STATUS: &str =
    "CYRENE Node Agent: Hardware Probing, Local Journaling & Core v1 session state online";
