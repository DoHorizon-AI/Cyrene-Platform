//! Compatibility exports for public adapter capability facts.
//!
//! The canonical owner is `cy-kernel-contract`; kernel-only ports continue
//! to live in `cy-kernel-api`.

pub use cy_kernel_contract::{
    CapabilityFact, EnforcementMode, EnforcementReport, NodeCapabilities,
};
