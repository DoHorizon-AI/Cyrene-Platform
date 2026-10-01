//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_workspace_product_adapters                              │
//! │  Role: Load private Product endpoint manifests for Connector hosts. │
//! │                                                                     │
//! │  模块职责：为 Connector host 加载私有 Product endpoint manifest。       │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Product request authorization and HTTP operation routing remain owned by
//! `cy-workspace-fabric`; this crate owns the private endpoint manifest and
//! secret-file loading boundary.

#![forbid(unsafe_code)]

mod endpoint_manifest;

pub use endpoint_manifest::{
    load_product_endpoint_configs, load_product_endpoint_configs_for_workspace,
    ProductEndpointManifestError,
};
