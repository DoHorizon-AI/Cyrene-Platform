//! Platform-owned Authority facade retained for source-compatible module paths.
//!
//! The Authority process implementation and durable lifecycle ports live under
//! the independent authority module. The former inline test service is not
//! registered by the production host.

pub use crate::authority::*;
