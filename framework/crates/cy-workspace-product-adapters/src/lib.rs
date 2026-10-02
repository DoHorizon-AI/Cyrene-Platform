//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_workspace_product_adapters                              │
//! │  Role: Dispatch catalog-routed Product HTTP calls safely.           │
//! │                                                                     │
//! │  模块职责：安全执行由 pinned owner catalog 路由的 Product HTTP 调用。   │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! This package depends on the small shared Product contract crate, never on
//! `cy-workspace-fabric`. Platform control-plane code supplies an opaque
//! authorized invocation; endpoint URLs and credentials come only from the
//! private server manifest. / 本 crate 不依赖大型 Fabric，也不接收 caller roles。

#![forbid(unsafe_code)]

mod endpoint;
mod endpoint_manifest;
mod http;

pub use endpoint::{
    validate_product_endpoint_configs, ProductEndpointConfig, ProductEndpointConfigError,
};
pub use endpoint_manifest::{
    load_product_endpoint_configs, load_product_endpoint_configs_for_workspace,
    ProductEndpointManifestError,
};
pub use http::ProductHttpApiAdapter;
