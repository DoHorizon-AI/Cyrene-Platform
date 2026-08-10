//! CYRENE Node Agent module.
//!
//! Provides local journaling, Core v1 session state, and self-upgrade protocol
//! support. Hardware discovery belongs to an external adapter process.

pub mod journal;
pub mod session;
pub mod upgrade;

pub use journal::*;
pub use session::*;
pub use upgrade::*;

pub const SCAFFOLD_STATUS: &str =
    "CYRENE Node Agent: Local Journaling & Core v1 session state online";
