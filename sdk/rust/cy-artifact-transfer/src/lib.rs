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
mod planner;
mod ticket;

pub use acquisition::{
    AcquisitionProvider, ExternalSource, HttpAcquisitionProvider, SourceImportJob, SourceSnapshot,
};
pub use contract::{
    ArtifactPeer, ArtifactPeerKind, ArtifactReplica, ArtifactSourceCandidate, TransferCheckpoint,
    TransferEstimate, TransferManifest, TransferPart, TransferPartSource, TransferPlan,
    TransferProtocol, TransferSession, TransferSource, TransferTicket,
};
pub use cy_manifest::{ArtifactKind, ArtifactRef};
pub use http::{HttpRangeTransfer, TransferError, TransferResult};
pub use planner::{
    ArtifactSourceResolver, ArtifactTransferCoordinator, InMemoryArtifactPeerDirectory,
    PeerSelectionPolicy, SeedFirstTransferPlanner, TransferPlanner, TransferPlanningRequest,
};
pub use ticket::{
    DevelopmentTransferTicketAuthority, TransferTicketIssuer, TransferTicketRequest,
    TransferTicketSigner, TransferTicketVerifier,
};

pub(crate) fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
