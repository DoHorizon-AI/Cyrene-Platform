//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_registry.rs                                              │
//! │  Module: cy_workspace_fabric::device_registry                      │
//! │  Role: Approved Workspace device certificate records.                │
//! │                                                                     │
//! │  模块职责：定义 Workspace 设备身份与审批状态的持久记录接口。             │
//! └─────────────────────────────────────────────────────────────────────┘

use crate::directory::WorkspaceDirectoryError;
use serde::{Deserialize, Serialize};

/// Authorization state for a certificate imported after external approval.
///
/// This state is data only; it does not itself authenticate a Relay session.
///
/// 一种状态记录不等同于 Relay 会话认证或凭据签发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceAuthorizationStatus {
    Approved,
    Revoked,
}

/// Stable Directory scope for one Workspace device identity.
///
/// 由组织、Workspace 与设备 ID 共同确定的稳定目录键。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDeviceKey {
    pub organization_id: String,
    pub workspace_id: String,
    pub device_id: String,
}

/// Metadata for an externally approved and issued Workspace device certificate.
///
/// Import happens only after an external approval and certificate-issuance
/// flow has completed. The fingerprint must be 64 lowercase hexadecimal
/// characters representing SHA-256 over the certificate DER. The store
/// validates its shape only; it does not parse or issue certificates.
///
/// 仅导入已经外部批准并签发的设备证书记录；存储接口只校验 DER 的 SHA-256 指纹格式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedWorkspaceDeviceCertificate {
    pub key: WorkspaceDeviceKey,
    pub certificate_fingerprint_sha256: String,
}

/// Persisted device identity, certificate fingerprint, and authorization state.
///
/// No private key, certificate body, or bearer credential is stored here.
///
/// 记录只保存身份、证书指纹和审批状态，不保存私钥、证书正文或 bearer credential。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDeviceRecord {
    pub key: WorkspaceDeviceKey,
    pub certificate_fingerprint_sha256: String,
    pub authorization_status: DeviceAuthorizationStatus,
}

/// Persistent Directory port for approved, externally issued Workspace device certificates.
///
/// Implementations only store externally approved certificate metadata and
/// revocation state. Approval decisions, credential issuance, and session
/// authentication remain separate responsibilities.
///
/// 此 port 只持久化已批准证书元数据与撤销状态；审批决策、短期凭据签发和会话认证仍属独立职责。
pub trait WorkspaceDeviceRegistry: Send + Sync {
    /// Import an externally approved and issued device certificate as approved,
    /// refusing duplicate identities or certificate fingerprints.
    ///
    /// `certificate_fingerprint_sha256` must be a lowercase SHA-256 hex digest.
    ///
    /// 仅在外部审批和签发完成后导入，初始状态为已批准；重复设备或证书指纹会失败。
    fn import_approved_device_certificate(
        &self,
        certificate: ApprovedWorkspaceDeviceCertificate,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError>;

    /// Revoke a stored approved certificate. Revocation is terminal for this
    /// record; a replacement certificate requires a future explicit rotation
    /// operation.
    ///
    /// 撤销已批准证书；此记录不可重新批准，证书轮换需由未来的显式操作处理。
    fn revoke_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<WorkspaceDeviceRecord, WorkspaceDirectoryError>;

    /// Read the record for one exact organization, Workspace, and device key.
    ///
    /// 读取精确组织、Workspace 与设备键对应的记录。
    fn find_device(
        &self,
        key: &WorkspaceDeviceKey,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError>;

    /// Find the unique record for a presented certificate fingerprint.
    ///
    /// The returned authorization status must be checked by any future
    /// certificate-authentication adapter; this lookup does not authenticate
    /// the peer itself.
    ///
    /// 按证书指纹查找唯一设备记录；未来认证适配器仍须检查状态并验证 TLS 对端。
    fn find_device_by_certificate_fingerprint(
        &self,
        fingerprint_sha256: &str,
    ) -> Result<Option<WorkspaceDeviceRecord>, WorkspaceDirectoryError>;
}
