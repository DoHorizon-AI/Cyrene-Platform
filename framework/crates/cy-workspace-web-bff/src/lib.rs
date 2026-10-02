// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/lib.rs              ║
// ║ Module: cy_workspace_web_bff                                        ║
// ║ Role: Composable authenticated same-origin Workspace Web BFF.       ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff                                          ║
// ║ 职责：可装配的已认证同源 Workspace Web BFF。                           ║
// ╚══════════════════════════════════════════════════════════════════════╝

#![forbid(unsafe_code)]

mod catalog_loader;
mod csrf;
mod device_approval;
mod fabric_device_approval;
mod fabric_gateway;
mod http;
mod problem;
mod product;

pub use catalog_loader::{
    load_product_catalog_and_snapshot, load_product_catalog_and_snapshot_from_environment,
    load_product_operation_catalog, load_product_operation_catalog_from_environment,
    CONTRACT_BUNDLE_MANIFEST_FILENAME, PRODUCT_CONTRACT_ROOT_ENV, PRODUCT_POLICY_BUNDLE_FILENAME,
};
pub use csrf::{csrf_cookie_name, csrf_cookie_path};
pub use device_approval::{
    DeviceApprovalCompletion, DeviceApprovalDependencies, DeviceApprovalScope,
    DeviceApprovalService, DeviceApprovalServiceError,
};
pub use fabric_device_approval::FabricDeviceApprovalAdapter;
pub use fabric_gateway::{
    FabricWorkspaceProductGateway, WorkspaceApiBinding, WorkspaceApiResolutionError,
    WorkspaceApiResolver,
};
pub use http::{
    router, router_with_device_approval, with_verified_web_session_routes, WebBffConfig,
    WebBffState, MAX_JSON_BODY_BYTES,
};
pub use problem::ProblemCode;
pub use product::{
    ProductCatalogError, ProductOperationCatalog, WorkspaceGatewayError, WorkspaceProductGateway,
};
