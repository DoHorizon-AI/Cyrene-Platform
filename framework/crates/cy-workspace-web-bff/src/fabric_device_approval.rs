// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: fabric_device_approval.rs                                      ║
// ║ Module: cy_workspace_web_bff::fabric_device_approval                ║
// ║ Role: Adapt verified BFF approvals to the Fabric authorization port. ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::fabric_device_approval                  ║
// ║ 职责：将已验证的 BFF 审批请求适配到 Fabric 授权端口。                 ║
// ╚══════════════════════════════════════════════════════════════════════╝

//! Explicit adapter from authenticated BFF approval requests to the
//! Workspace device-enrollment authorization port.
//!
//! This module never derives identity or role from request JSON. The only
//! identity input is the verified web principal; browser scope is passed only
//! after the BFF route has checked its organization and Directory capability.
//!
//! 将已认证的 BFF 审批请求显式适配到 Workspace device-enrollment 授权端口。本模块不从请求 JSON 派生身份或角色；
//! identity 仅来自已验证 web principal。浏览器 scope 仅在 BFF 路由检查 organization 与 Directory capability 后传入。

use std::sync::Arc;

use async_trait::async_trait;
use cy_proto::workspace_v1::UserIdentityRef;
use cy_workspace_control_plane::VerifiedWebPrincipal;
use cy_workspace_postgres_storage::{
    device_authorization::DeviceAuthorizationScope, DeviceEnrollmentAuthorizationPort,
    DeviceEnrollmentHttpError, SecretBytes,
};
use serde_json::Value;

use crate::{
    DeviceApprovalCompletion, DeviceApprovalScope, DeviceApprovalService,
    DeviceApprovalServiceError,
};

/// BFF adapter backed by one explicitly supplied Workspace authorization port.
///
/// Hosts may inject this adapter only after constructing the real authorization
/// port and its required providers. It does not create or substitute those
/// providers itself.
///
/// 由调用方显式注入一个 Workspace authorization port 的 BFF adapter。Host 只有在真实 authorization port 与所需 provider
/// 均已构造后才能注入；本 adapter 不会自行创建或替代 provider。
pub struct FabricDeviceApprovalAdapter {
    port: Arc<dyn DeviceEnrollmentAuthorizationPort>,
}

impl FabricDeviceApprovalAdapter {
    /// Binds the adapter to the supplied Workspace authorization port.
    ///
    /// 将 adapter 绑定到调用方提供的 Workspace authorization port。
    pub fn new(port: Arc<dyn DeviceEnrollmentAuthorizationPort>) -> Self {
        Self { port }
    }
}

#[async_trait]
impl DeviceApprovalService for FabricDeviceApprovalAdapter {
    async fn begin(
        &self,
        principal: &VerifiedWebPrincipal,
        abuse_key: [u8; 32],
        user_code: &str,
        scope: &DeviceApprovalScope,
    ) -> Result<Value, DeviceApprovalServiceError> {
        let identity = verified_identity(principal);
        let scope = fabric_scope(principal, scope)?;
        self.port
            .begin_approval(&identity, &abuse_key, user_code, scope)
            .await
            .map_err(map_fabric_error)
    }

    async fn complete(
        &self,
        principal: &VerifiedWebPrincipal,
        approval_id: &str,
        assertion: Option<Value>,
    ) -> Result<DeviceApprovalCompletion, DeviceApprovalServiceError> {
        let identity = verified_identity(principal);
        let assertion = assertion
            .map(SecretBytes::from_webauthn_assertion_json)
            .transpose()
            .map_err(map_fabric_error)?;
        let completion = self
            .port
            .complete_approval(&identity, approval_id.to_owned(), assertion)
            .await
            .map_err(map_fabric_error)?;

        Ok(DeviceApprovalCompletion {
            body: completion.body,
            accepted: completion.accepted,
        })
    }

    async fn deny(
        &self,
        principal: &VerifiedWebPrincipal,
        abuse_key: [u8; 32],
        user_code: &str,
        scope: &DeviceApprovalScope,
    ) -> Result<Value, DeviceApprovalServiceError> {
        let identity = verified_identity(principal);
        let scope = fabric_scope(principal, scope)?;
        self.port
            .deny(&identity, &abuse_key, user_code, scope)
            .await
            .map_err(map_fabric_error)
    }
}

fn verified_identity(principal: &VerifiedWebPrincipal) -> UserIdentityRef {
    principal.identity().clone()
}

fn fabric_scope(
    principal: &VerifiedWebPrincipal,
    scope: &DeviceApprovalScope,
) -> Result<DeviceAuthorizationScope, DeviceApprovalServiceError> {
    if scope.organization_id != principal.organization_id() {
        return Err(DeviceApprovalServiceError::Forbidden);
    }

    Ok(DeviceAuthorizationScope {
        organization_id: scope.organization_id.clone(),
        workspace_id: scope.workspace_id.clone(),
    })
}

fn map_fabric_error(error: DeviceEnrollmentHttpError) -> DeviceApprovalServiceError {
    match error {
        DeviceEnrollmentHttpError::InvalidRequest => DeviceApprovalServiceError::InvalidRequest,
        DeviceEnrollmentHttpError::Unauthorized | DeviceEnrollmentHttpError::Forbidden => {
            DeviceApprovalServiceError::Forbidden
        }
        DeviceEnrollmentHttpError::InvalidGrant | DeviceEnrollmentHttpError::Expired => {
            DeviceApprovalServiceError::NotFound
        }
        DeviceEnrollmentHttpError::Conflict => DeviceApprovalServiceError::Conflict,
        DeviceEnrollmentHttpError::RateLimited { .. }
        | DeviceEnrollmentHttpError::FirstStartQuotaExceeded => {
            DeviceApprovalServiceError::RateLimited
        }
        DeviceEnrollmentHttpError::Unavailable => DeviceApprovalServiceError::Unavailable,
    }
}
