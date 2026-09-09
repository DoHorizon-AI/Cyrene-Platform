//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 ticket.rs                                                       │
//! │  Module: cy_artifact_transfer::ticket                               │
//! │  Role: Replaceable TransferTicket signing and verification seam.    │
//! │                                                                     │
//! │  模块职责：提供可替换的 TransferTicket 签发与校验接口及开发实现。         │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;

use cy_manifest::ArtifactRef;
use sha2::{Digest, Sha256};

use crate::{TransferError, TransferTicket};

/// Artifact-plane ticket signer. Production may replace the development key.
pub trait TransferTicketSigner: Send + Sync {
    fn sign(&self, ticket: &mut TransferTicket) -> Result<(), TransferError>;
}

/// Destination/source verification seam for short-lived tickets.
pub trait TransferTicketVerifier: Send + Sync {
    fn verify(&self, ticket: &TransferTicket) -> Result<(), TransferError>;
}

/// Artifact-plane issuer used after policy and source selection complete.
pub trait TransferTicketIssuer: Send + Sync {
    fn issue(&self, request: TransferTicketRequest) -> Result<TransferTicket, TransferError>;
}

/// Destination- and part-scoped request for one short-lived transfer ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferTicketRequest {
    pub ticket_id: String,
    pub artifact: ArtifactRef,
    pub source_peer_id: String,
    pub destination_peer_id: String,
    pub allowed_parts: BTreeSet<u32>,
    pub expires_at_unix_ms: u64,
    pub max_bytes: u64,
}

/// Deterministic reference signer for development and conformance fixtures.
#[derive(Debug, Clone)]
pub struct DevelopmentTransferTicketAuthority {
    key: Vec<u8>,
}

impl DevelopmentTransferTicketAuthority {
    pub fn new(key: impl AsRef<[u8]>) -> Result<Self, TransferError> {
        let key = key.as_ref().to_vec();
        if key.len() < 32 {
            return Err(TransferError::Contract(
                "development ticket key must contain at least 32 bytes".to_string(),
            ));
        }
        Ok(Self { key })
    }

    fn signature(&self, ticket: &TransferTicket) -> Result<String, TransferError> {
        let mut unsigned = ticket.clone();
        unsigned.signature.clear();
        let payload = serde_json::to_vec(&unsigned)
            .map_err(|error| TransferError::Contract(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(&self.key);
        hasher.update([0]);
        hasher.update(payload);
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }
}

impl TransferTicketSigner for DevelopmentTransferTicketAuthority {
    fn sign(&self, ticket: &mut TransferTicket) -> Result<(), TransferError> {
        ticket.signature = self.signature(ticket)?;
        Ok(())
    }
}

impl TransferTicketVerifier for DevelopmentTransferTicketAuthority {
    fn verify(&self, ticket: &TransferTicket) -> Result<(), TransferError> {
        ticket.validate()?;
        if ticket.signature != self.signature(ticket)? {
            return Err(TransferError::Authorization(
                "TransferTicket signature is invalid".to_string(),
            ));
        }
        Ok(())
    }
}

impl TransferTicketIssuer for DevelopmentTransferTicketAuthority {
    fn issue(&self, request: TransferTicketRequest) -> Result<TransferTicket, TransferError> {
        let mut ticket = TransferTicket {
            ticket_id: request.ticket_id,
            artifact: request.artifact,
            source_peer_id: request.source_peer_id,
            destination_peer_id: request.destination_peer_id,
            allowed_parts: request.allowed_parts,
            expires_at_unix_ms: request.expires_at_unix_ms,
            max_bytes: request.max_bytes,
            signature: String::new(),
        };
        self.sign(&mut ticket)?;
        ticket.validate()?;
        Ok(ticket)
    }
}

#[cfg(test)]
mod tests {
    use cy_manifest::{ArtifactKind, ArtifactRef};

    use super::*;

    fn artifact() -> ArtifactRef {
        ArtifactRef {
            uri: format!("artifact://sha256/{}", "1".repeat(64)),
            digest: format!("sha256:{}", "1".repeat(64)),
            size_bytes: 4,
            kind: ArtifactKind::generic(),
            manifest_digest: None,
        }
    }

    #[test]
    fn development_ticket_detects_scope_tampering() {
        let authority = DevelopmentTransferTicketAuthority::new([7_u8; 32]).unwrap();
        let mut ticket = TransferTicket {
            ticket_id: "ticket-1".to_string(),
            artifact: artifact(),
            source_peer_id: "seed-1".to_string(),
            destination_peer_id: "runtime-1".to_string(),
            allowed_parts: BTreeSet::from([0]),
            expires_at_unix_ms: u64::MAX,
            max_bytes: 4,
            signature: String::new(),
        };
        authority.sign(&mut ticket).unwrap();
        authority.verify(&ticket).unwrap();
        ticket.destination_peer_id = "attacker".to_string();
        assert!(matches!(
            authority.verify(&ticket),
            Err(TransferError::Authorization(_))
        ));
    }
}
