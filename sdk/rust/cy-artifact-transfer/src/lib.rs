//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_artifact_transfer                                       │
//! │  Role: Resumable HTTPS Range transfer around Artifact identity.     │
//! │                                                                     │
//! │  模块职责：围绕 canonical Artifact 身份实现可校验、可恢复的 HTTPS 传输。 │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod acquisition;
mod contract;
mod http;

pub use acquisition::{AcquisitionProvider, ExternalSource, SourceImportJob, SourceSnapshot};
pub use contract::{
    ArtifactIdentity, ArtifactReplica, TransferCheckpoint, TransferManifest, TransferPart,
    TransferSession,
};
pub use http::{HttpRangeTransfer, TransferError, TransferResult};
