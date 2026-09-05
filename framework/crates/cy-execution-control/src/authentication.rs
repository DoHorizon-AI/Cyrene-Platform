//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 authentication.rs                                               │
//! │  Module: cy_execution_control::authentication                       │
//! │  Role: Bind mTLS peers to Host or Runtime identities.               │
//! │                                                                     │
//! │  模块职责：把 mTLS peer 绑定到 Host 或 Runtime identity。             │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeMap;

use cy_kernel_contract::Identity;
use cy_proto::core_v1::NodeRef;
use sha2::{Digest, Sha256};
use tonic::metadata::MetadataMap;

use crate::DispatchError;

/// Identity established by the transport boundary before parsing a Hello.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthenticatedAgent {
    Host { node: NodeRef },
    Runtime { runtime: Identity, node: NodeRef },
}

/// Replaceable certificate-to-agent binding. Implementations must fail closed.
pub trait PeerAuthenticator: Send + Sync {
    fn authenticate(
        &self,
        certificate_chain: &[Vec<u8>],
        metadata: &MetadataMap,
    ) -> Result<AuthenticatedAgent, DispatchError>;
}

/// Exact SHA-256 certificate bindings for small, statically provisioned fleets.
///
/// This is suitable for Alpha deployments and tests. Certificate issuance and
/// rotation policy remain outside this execution-control component.
#[derive(Debug, Clone, Default)]
pub struct CertificateFingerprintAuthenticator {
    bindings: BTreeMap<[u8; 32], AuthenticatedAgent>,
}

impl CertificateFingerprintAuthenticator {
    pub fn new(
        bindings: impl IntoIterator<Item = (Vec<u8>, AuthenticatedAgent)>,
    ) -> Result<Self, DispatchError> {
        let mut indexed = BTreeMap::new();
        for (certificate, agent) in bindings {
            if certificate.is_empty() {
                return Err(DispatchError::input(
                    "PEER_CERTIFICATE_INVALID",
                    "certificate binding cannot be empty",
                ));
            }
            let digest: [u8; 32] = Sha256::digest(certificate).into();
            if indexed.insert(digest, agent).is_some() {
                return Err(DispatchError::input(
                    "PEER_CERTIFICATE_DUPLICATE",
                    "one certificate cannot authenticate multiple Agent identities",
                ));
            }
        }
        if indexed.is_empty() {
            return Err(DispatchError::input(
                "PEER_BINDINGS_REQUIRED",
                "at least one certificate binding is required",
            ));
        }
        Ok(Self { bindings: indexed })
    }
}

impl PeerAuthenticator for CertificateFingerprintAuthenticator {
    fn authenticate(
        &self,
        certificate_chain: &[Vec<u8>],
        _metadata: &MetadataMap,
    ) -> Result<AuthenticatedAgent, DispatchError> {
        let leaf = certificate_chain.first().ok_or_else(|| {
            DispatchError::input(
                "PEER_CERTIFICATE_REQUIRED",
                "NodeControl requires an authenticated mTLS client certificate",
            )
        })?;
        let digest: [u8; 32] = Sha256::digest(leaf).into();
        self.bindings.get(&digest).cloned().ok_or_else(|| {
            DispatchError::input(
                "PEER_CERTIFICATE_UNAUTHORIZED",
                "client certificate is not bound to an enrolled Agent identity",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_leaf_certificate_selects_one_bound_agent() {
        let agent = AuthenticatedAgent::Host {
            node: NodeRef {
                node_id: "node-1".to_string(),
                node_epoch: 7,
            },
        };
        let authenticator =
            CertificateFingerprintAuthenticator::new([(b"leaf-der".to_vec(), agent.clone())])
                .unwrap();

        assert_eq!(
            authenticator
                .authenticate(&[b"leaf-der".to_vec()], &MetadataMap::new())
                .unwrap(),
            agent
        );
        assert_eq!(
            authenticator
                .authenticate(&[], &MetadataMap::new())
                .unwrap_err()
                .reason_code,
            "PEER_CERTIFICATE_REQUIRED"
        );
    }

    #[test]
    fn one_certificate_cannot_bind_two_identities() {
        let error = CertificateFingerprintAuthenticator::new([
            (
                b"same-leaf".to_vec(),
                AuthenticatedAgent::Host {
                    node: NodeRef {
                        node_id: "node-1".to_string(),
                        node_epoch: 7,
                    },
                },
            ),
            (
                b"same-leaf".to_vec(),
                AuthenticatedAgent::Runtime {
                    runtime: Identity {
                        id: "runtime-1".to_string(),
                        generation: 1,
                    },
                    node: NodeRef {
                        node_id: "node-1".to_string(),
                        node_epoch: 7,
                    },
                },
            ),
        ])
        .unwrap_err();
        assert_eq!(error.reason_code, "PEER_CERTIFICATE_DUPLICATE");
    }
}
