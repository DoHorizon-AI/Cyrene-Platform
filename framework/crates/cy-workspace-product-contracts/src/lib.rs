//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Product contract and catalog policy API                            │
//! │  Module: cy_workspace_product_contracts                             │
//! │  Role: Load pinned owner catalogs and authorize generic Product calls.│
//! │                                                                     │
//! │  模块职责：装载固定来源的 owner 目录，并按 Platform 独立策略授权调用。 │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! Product owners publish route and schema metadata. Platform separately
//! publishes the trusted authorization policy. Callers select only an owner
//! and operation; they do not supply routes, credentials, roles, or catalog
//! versions. / Product 所有者发布路由和 schema；授权策略仍由 Platform 独立管理。

#![forbid(unsafe_code)]

/// Semantic version of the shared Rust Product v2 contract API.
pub const CONTRACT_API_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Product-scoped alias for [`CONTRACT_API_VERSION`].
pub const PRODUCT_CONTRACT_API_VERSION: &str = CONTRACT_API_VERSION;

mod bundle;
mod invocation;
mod policy;
mod strict_json;

pub use bundle::{
    CatalogError, IdempotencyConstraints, JsonScopeBinding, MatchContextField, ProductBundlePins,
    ProductContractBundle, ProductOperation, ProductOperationKind, ResourceIdConstraints,
    ScopeBindings,
};
pub use invocation::{
    AuthorizedProductInvocation, ProductInvocationAdapter, ProductInvocationError,
    ProductInvocationResponse, ResolvedProductRoute,
};
pub use policy::{
    AuthorizationError, ProductPrincipalKind, TrustedProductPolicy, TrustedWorkspaceScope,
    VerifiedProductPrincipal,
};
pub use strict_json::{
    parse_json_bytes, parse_json_bytes_with_limit, StrictJsonError, PRODUCT_JSON_BYTES_LIMIT,
};
