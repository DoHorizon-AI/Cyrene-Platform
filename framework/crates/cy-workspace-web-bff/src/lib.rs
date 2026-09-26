// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/lib.rs              ║
// ║ Module: cy_workspace_web_bff                                        ║
// ║ Role: Composable authenticated same-origin Workspace Web BFF.       ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff                                          ║
// ║ 职责：可装配的已认证同源 Workspace Web BFF。                           ║
// ╚══════════════════════════════════════════════════════════════════════╝

#![forbid(unsafe_code)]

mod csrf;
mod fabric_gateway;
mod http;
mod manifest;
mod problem;
mod product;

pub use csrf::{csrf_cookie_name, csrf_cookie_path};
pub use fabric_gateway::{
    FabricWorkspaceProductGateway, WorkspaceApiBinding, WorkspaceApiResolutionError,
    WorkspaceApiResolver,
};
pub use http::{router, WebBffConfig, WebBffState, MAX_JSON_BODY_BYTES};
pub use manifest::{
    product_projection_manifest, ProductProjectionEntry, ProductProjectionManifestError,
};
pub use problem::ProblemCode;
pub use product::{
    ProductCatalogError, ProductJsonSchema, ProductOperationCatalog, ProductOperationContract,
    ProductPathParameter, ProductRequestBodyContract, ProductResourceReferenceField,
    ProductResponseContract, WorkspaceGatewayError, WorkspaceProductGateway,
};
