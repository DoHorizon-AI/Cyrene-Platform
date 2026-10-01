//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 device_registry.rs                                              │
//! │  Module: cy_workspace_control_plane::device_registry                      │
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

/// Current active certificate facts required to authenticate a Relay peer.
///
/// A registry implementation must return this record only when the certificate
/// is active, its authorization is Delivered, and its registration binding and
/// generation still match the current Directory identity. Implementations must
/// fail closed when that current-state check is unavailable.
///
/// Relay 对端认证所需的当前有效证书事实；适配器必须核对已交付状态及当前 binding/generation。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDeviceCertificateIdentity {
    pub key: WorkspaceDeviceKey,
    pub certificate_fingerprint_sha256: String,
    pub registration_binding_id: [u8; 16],
    pub authorization_generation: u64,
    pub csr_sha256: [u8; 32],
    pub spki_sha256: [u8; 32],
    pub serial_number: Vec<u8>,
    pub not_after_unix_ms: u64,
}

/// RAII guard that keeps a current Registry identity fenced until Relay queue admission ends.
///
/// Implementations release the fence when this value is dropped. Non-durable registries do not
/// provide this guarantee and must leave the acquisition method below fail-closed.
///
/// 持有该 guard 会阻止 Registry 撤销或 Directory 代次变更越过本次本地 Relay 入队；释放由 Drop 完成。
pub trait WorkspaceDeviceDispatchFence: Send {}

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

    /// Find the current active device certificate and its immutable binding tuple.
    ///
    /// The default refuses the lookup because a basic fingerprint record cannot
    /// prove current generation or binding state.
    ///
    /// 按指纹读取当前有效证书和不可变绑定元组；基础记录无法证明当前代次，默认拒绝。
    fn find_current_device_certificate_identity(
        &self,
        _fingerprint_sha256: &str,
    ) -> Result<Option<WorkspaceDeviceCertificateIdentity>, WorkspaceDirectoryError> {
        Err(WorkspaceDirectoryError::Storage(
            "Current Workspace device certificate identity is unavailable".to_owned(),
        ))
    }

    /// Acquire a Registry transaction guard for one final Workspace Relay queue admission.
    ///
    /// The default denies admission because an ordinary current-state read cannot fence a later
    /// revocation. Durable implementations must revalidate the complete identity while holding
    /// the guard and keep it until the caller synchronously enqueues or abandons the frame.
    ///
    /// 默认拒绝派发；普通状态读取不能阻止随后发生的撤销。持久实现必须在 guard 生命周期内重验完整身份。
    fn acquire_relay_dispatch_fence(
        &self,
        _expected: &WorkspaceDeviceCertificateIdentity,
    ) -> Result<Box<dyn WorkspaceDeviceDispatchFence>, WorkspaceDirectoryError> {
        Err(WorkspaceDirectoryError::Storage(
            "Workspace Relay dispatch admission fence is unavailable".to_owned(),
        ))
    }
}
