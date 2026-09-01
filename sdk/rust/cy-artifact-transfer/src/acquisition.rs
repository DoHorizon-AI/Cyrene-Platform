//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 acquisition.rs                                                  │
//! │  Module: cy_artifact_transfer::acquisition                          │
//! │  Role: External-source to immutable-Artifact provider seam.         │
//! │                                                                     │
//! │  模块职责：把外部可变来源导入为内部不可变 Artifact 的 Provider seam。    │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_kernel_contract::Identity;
use serde::{Deserialize, Serialize};

use crate::{ArtifactIdentity, TransferError};

/// Provider-specific mutable source request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalSource {
    pub provider: String,
    pub locator: String,
    pub revision: Option<String>,
}

/// Generic import Operation associated with one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceImportJob {
    pub operation: Identity,
    pub source: ExternalSource,
}

/// Immutable result of importing an external source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    pub source: ExternalSource,
    pub artifact: ArtifactIdentity,
}

/// Replaceable GitHub, Hugging Face, mirror, or cloud acquisition boundary.
pub trait AcquisitionProvider: Send + Sync {
    fn import(&self, job: &SourceImportJob) -> Result<SourceSnapshot, TransferError>;
}
