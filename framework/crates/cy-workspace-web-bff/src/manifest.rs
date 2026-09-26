// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/manifest.rs        ║
// ║ Module: cy_workspace_web_bff::manifest                             ║
// ║ Role: Bind generated Product TCK rows to the Workspace wire enums. ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::manifest                               ║
// ║ 职责：将生成的 Product TCK 行绑定到 Workspace wire enum。             ║
// ╚══════════════════════════════════════════════════════════════════════╝

use cy_workspace_fabric::workspace_v1::{
    WorkspaceProductApiOperation, WorkspaceProductApiOwner, WorkspaceProductApiRequestKind,
};
use thiserror::Error;

struct GeneratedProjectionRow {
    owner: &'static str,
    wire_operation: &'static str,
    product_operation_id: &'static str,
    kind: &'static str,
    product_contract: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/product_projection_manifest.rs"));

/// One immutable operation mapping read from the canonical Product TCK.
///
/// 从规范 Product TCK 读取的一条不可变 operation 映射。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductProjectionEntry {
    /// Product authority named by the TCK row.
    pub owner: WorkspaceProductApiOwner,
    /// Closed Workspace wire operation key.
    pub operation: WorkspaceProductApiOperation,
    /// Product OpenAPI `operationId` from the same TCK row.
    pub product_operation_id: &'static str,
    /// Semantic request kind declared by the TCK row.
    pub kind: WorkspaceProductApiRequestKind,
    /// Repository-relative Product contract path recorded in the TCK.
    pub product_contract: &'static str,
}

/// Failure to reconcile the generated TCK rows with the compiled Workspace enum.
///
/// 无法使生成的 TCK 行与编译后的 Workspace enum 保持一致。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProductProjectionManifestError {
    /// A canonical name is absent from the generated Workspace bindings.
    #[error("canonical Product projection name is absent from the Workspace enum")]
    EnumMismatch,
}

/// Return TCK rows after resolving every name through generated Workspace enums.
///
/// 使用生成的 Workspace enum 解析每个名称后，返回 TCK 行。
pub fn product_projection_manifest(
) -> Result<Vec<ProductProjectionEntry>, ProductProjectionManifestError> {
    let mut rows = Vec::with_capacity(GENERATED_PRODUCT_PROJECTION_ROWS.len());
    for row in GENERATED_PRODUCT_PROJECTION_ROWS {
        let owner = WorkspaceProductApiOwner::from_str_name(&format!(
            "WORKSPACE_PRODUCT_API_OWNER_{}",
            row.owner
        ))
        .ok_or(ProductProjectionManifestError::EnumMismatch)?;
        let operation = WorkspaceProductApiOperation::from_str_name(row.wire_operation)
            .ok_or(ProductProjectionManifestError::EnumMismatch)?;
        let kind = WorkspaceProductApiRequestKind::from_str_name(&format!(
            "WORKSPACE_PRODUCT_API_REQUEST_KIND_{}",
            row.kind
        ))
        .ok_or(ProductProjectionManifestError::EnumMismatch)?;
        rows.push(ProductProjectionEntry {
            owner,
            operation,
            product_operation_id: row.product_operation_id,
            kind,
            product_contract: row.product_contract,
        });
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_projection_manifest_matches_workspace_enum_exactly() {
        let manifest = product_projection_manifest().expect("TCK and wire enum should match");
        assert_eq!(manifest.len(), 13);
        assert_eq!(
            manifest
                .iter()
                .filter(|entry| entry.kind == WorkspaceProductApiRequestKind::Read)
                .count(),
            7
        );
        assert_eq!(
            manifest
                .iter()
                .filter(|entry| entry.kind == WorkspaceProductApiRequestKind::Command)
                .count(),
            6
        );
    }
}
