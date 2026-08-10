//! CYRENE Node Agent module.
//!
//! Provides local journaling, Core v1 session state, and self-upgrade protocol
//! support. Hardware discovery belongs to an external adapter process.

pub mod bridge;
pub mod daemon;
pub mod journal;
pub mod session;
pub mod upgrade;

pub use bridge::*;
pub use daemon::*;
pub use journal::*;
pub use session::*;
pub use upgrade::*;

pub const SCAFFOLD_STATUS: &str =
    "CYRENE Node Agent: outbound mTLS, fenced Core v1 session, and local Kernel UDS bridge online";
