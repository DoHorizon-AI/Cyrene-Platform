//! Compatibility exports for the public adapter binding facts.
//!
//! The canonical owner is `cy-kernel-contract`. This module remains so
//! kernel-internal crates can migrate without changing the Frozen v1 names.

pub use cy_kernel_contract::{DeviceBinding, EnvironmentMerge};
