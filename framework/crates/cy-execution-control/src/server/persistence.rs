use super::*;
use serde::{Deserialize, Serialize};

type Identity = (String, u64);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    host_tokens: Vec<(Identity, String)>,
    runtime_credentials: Vec<RuntimeCredential>,
    runtime_bindings: Vec<(Identity, Identity)>,
    highest_epochs: BTreeMap<String, u64>,
    assignments: Vec<AcceptedAssignment>,
    #[serde(default)]
    terminal_observations: Vec<TerminalObservation>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeCredential {
    runtime: Identity,
    node: Identity,
    token: String,
    workload: Identity,
    organization: String,
    workspace: String,
    expires_at: u64,
    pending_proof_digest: Option<[u8; 32]>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedAssignment {
    id: String,
    #[serde(default)]
    attempt_id: String,
    runtime: Identity,
    node: Identity,
    lease: Identity,
    fence: u64,
    digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalObservation {
    assignment_id: String,
    value: Vec<u8>,
}
fn identity(value: &semantic::Identity) -> Identity {
    (value.id.clone(), value.generation)
}
fn node(value: &NodeKey) -> Identity {
    (value.node_id.clone(), value.node_epoch)
}
fn restore_identity(value: Identity) -> Result<semantic::Identity, DispatchError> {
    let result = semantic::Identity {
        id: value.0,
        generation: value.1,
    };
    result.validate().map_err(|_| invalid())?;
    Ok(result)
}
fn restore_node(value: Identity) -> Result<NodeKey, DispatchError> {
    let value = restore_identity(value)?;
    Ok(NodeKey {
        node_id: value.id,
        node_epoch: value.generation,
    })
}
fn invalid() -> DispatchError {
    DispatchError::input(
        "SESSION_SNAPSHOT_INVALID",
        "persisted execution session state is invalid; original data retained",
    )
}

impl Registry {
    pub(super) fn snapshot(&self) -> Result<Vec<u8>, DispatchError> {
        let runtime_credentials = self
            .runtime_resume_tokens
            .iter()
            .map(|(key, token)| {
                let grant = self.runtime_grants.get(key).ok_or_else(invalid)?;
                Ok(RuntimeCredential {
                    runtime: identity(&key.runtime),
                    node: node(&key.node),
                    token: token.clone(),
                    workload: identity(&grant.workload_identity),
                    organization: grant.scope.organization_id.clone(),
                    workspace: grant.scope.workspace_id.clone(),
                    expires_at: grant.expires_at_unix_ms,
                    pending_proof_digest: self
                        .pending_runtime_enrollments
                        .get(key)
                        .map(|p| p.proof_digest),
                })
            })
            .collect::<Result<Vec<_>, DispatchError>>()?;
        let snapshot = Snapshot {
            version: 1,
            host_tokens: self
                .host_resume_tokens
                .iter()
                .map(|(key, token)| (node(key), token.clone()))
                .collect(),
            runtime_credentials,
            runtime_bindings: self
                .runtime_node_bindings
                .iter()
                .map(|(runtime, key)| (identity(runtime), node(key)))
                .collect(),
            highest_epochs: self.highest_node_epochs.clone(),
            assignments: self
                .accepted_assignments
                .values()
                .map(|a| AcceptedAssignment {
                    id: a.assignment_id.clone(),
                    attempt_id: a.attempt_id.clone(),
                    runtime: identity(&a.runtime),
                    node: node(&a.node),
                    lease: identity(&a.lease.identity),
                    fence: a.lease.fence_token,
                    digest: a.digest,
                })
                .collect(),
            terminal_observations: self
                .terminal_observations
                .iter()
                .map(|(assignment_id, value)| TerminalObservation {
                    assignment_id: assignment_id.clone(),
                    value: value.clone(),
                })
                .collect(),
        };
        serde_json::to_vec(&snapshot).map_err(|_| invalid())
    }
    pub(super) fn restore(bytes: &[u8]) -> Result<Self, DispatchError> {
        let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if snapshot.version != 1 {
            return Err(invalid());
        }
        let mut registry = Self::default();
        for (id, epoch) in snapshot.highest_epochs {
            restore_identity((id.clone(), epoch))?;
            registry.highest_node_epochs.insert(id, epoch);
        }
        let check_node = |node: &NodeKey| -> Result<(), DispatchError> {
            if registry
                .highest_node_epochs
                .get(&node.node_id)
                .is_none_or(|epoch| node.node_epoch > *epoch)
            {
                return Err(invalid());
            }
            Ok(())
        };
        for (key, token) in snapshot.host_tokens {
            let key = restore_node(key)?;
            check_node(&key)?;
            if token.is_empty() || registry.host_resume_tokens.insert(key, token).is_some() {
                return Err(invalid());
            }
        }
        for credential in snapshot.runtime_credentials {
            let key = RuntimeKey {
                runtime: restore_identity(credential.runtime)?,
                node: restore_node(credential.node)?,
            };
            check_node(&key.node)?;
            if credential.token.is_empty()
                || credential.organization.is_empty()
                || credential.workspace.is_empty()
                || credential.expires_at == 0
            {
                return Err(invalid());
            }
            let grant = EnrollmentGrant {
                workload_identity: restore_identity(credential.workload)?,
                scope: RuntimeScope {
                    organization_id: credential.organization,
                    workspace_id: credential.workspace,
                    runtime: key.runtime.clone(),
                },
                expires_at_unix_ms: credential.expires_at,
            };
            if registry
                .runtime_resume_tokens
                .insert(key.clone(), credential.token.clone())
                .is_some()
            {
                return Err(invalid());
            }
            registry.runtime_grants.insert(key.clone(), grant.clone());
            if let Some(proof_digest) = credential.pending_proof_digest {
                registry.pending_runtime_enrollments.insert(
                    key,
                    PendingRuntimeEnrollment {
                        proof_digest,
                        grant,
                        resume_token: credential.token,
                    },
                );
            }
        }
        for (runtime, key) in snapshot.runtime_bindings {
            let runtime = restore_identity(runtime)?;
            let key = restore_node(key)?;
            check_node(&key)?;
            if registry
                .runtime_node_bindings
                .insert(runtime, key)
                .is_some()
            {
                return Err(invalid());
            }
        }
        for value in snapshot.assignments {
            let binding = AssignmentBinding {
                assignment_id: value.id,
                attempt_id: value.attempt_id,
                runtime: restore_identity(value.runtime)?,
                node: restore_node(value.node)?,
                lease: LeaseKey {
                    identity: restore_identity(value.lease)?,
                    fence_token: value.fence,
                },
                digest: value.digest,
            };
            check_node(&binding.node)?;
            if binding.assignment_id.is_empty()
                || binding.lease.fence_token == 0
                || registry.runtime_node_bindings.get(&binding.runtime) != Some(&binding.node)
                || registry
                    .accepted_assignments
                    .insert(binding.assignment_id.clone(), binding)
                    .is_some()
            {
                return Err(invalid());
            }
        }
        for terminal in snapshot.terminal_observations {
            let value = core_v1::RuntimeObservation::decode(terminal.value.as_slice())
                .map_err(|_| invalid())?;
            let state = core_v1::RuntimeObservedState::try_from(value.observed_state)
                .map_err(|_| invalid())?;
            let termination = core_v1::TerminationClassification::try_from(value.termination)
                .map_err(|_| invalid())?;
            if terminal.assignment_id.is_empty()
                || value.assignment_id != terminal.assignment_id
                || value.attempt_id.is_empty()
                || value
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.identity.as_ref())
                    .is_none()
                || !matches!(
                    state,
                    core_v1::RuntimeObservedState::Stopped
                        | core_v1::RuntimeObservedState::Failed
                        | core_v1::RuntimeObservedState::Lost
                )
                || termination == core_v1::TerminationClassification::Unspecified
                || registry
                    .terminal_observations
                    .insert(terminal.assignment_id, terminal.value)
                    .is_some()
            {
                return Err(invalid());
            }
        }
        // Live streams, pending oneshots and readiness are intentionally absent.
        // Both Host and Runtime must authenticate and establish fresh sessions.
        Ok(registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_credentials_bindings_and_fences_without_live_sessions() {
        let mut original = Registry::default();
        let key = NodeKey {
            node_id: "host".into(),
            node_epoch: 3,
        };
        let runtime = semantic::Identity {
            id: "runtime".into(),
            generation: 2,
        };
        let runtime_key = RuntimeKey {
            runtime: runtime.clone(),
            node: key.clone(),
        };
        let grant = EnrollmentGrant {
            workload_identity: semantic::Identity {
                id: "workload".into(),
                generation: 2,
            },
            scope: RuntimeScope {
                organization_id: "org".into(),
                workspace_id: "workspace".into(),
                runtime: runtime.clone(),
            },
            expires_at_unix_ms: u64::MAX,
        };
        original.highest_node_epochs.insert("host".into(), 3);
        original
            .host_resume_tokens
            .insert(key.clone(), "host-token".into());
        original
            .runtime_node_bindings
            .insert(runtime.clone(), key.clone());
        original
            .runtime_resume_tokens
            .insert(runtime_key.clone(), "runtime-token".into());
        original
            .runtime_grants
            .insert(runtime_key.clone(), grant.clone());
        original.pending_runtime_enrollments.insert(
            runtime_key.clone(),
            PendingRuntimeEnrollment {
                proof_digest: [7; 32],
                grant,
                resume_token: "runtime-token".into(),
            },
        );
        original.accepted_assignments.insert(
            "assignment".into(),
            AssignmentBinding {
                assignment_id: "assignment".into(),
                attempt_id: "attempt".into(),
                runtime,
                node: key,
                lease: LeaseKey {
                    identity: semantic::Identity {
                        id: "lease".into(),
                        generation: 1,
                    },
                    fence_token: 9,
                },
                digest: [3; 32],
            },
        );
        original.terminal_observations.insert(
            "assignment".into(),
            core_v1::RuntimeObservation {
                runtime: Some(core_v1::RuntimeRef {
                    identity: Some(semantic_v1::Identity {
                        id: "runtime".into(),
                        generation: 2,
                    }),
                }),
                observed_state: core_v1::RuntimeObservedState::Stopped as i32,
                termination: core_v1::TerminationClassification::External as i32,
                reason_code: "WORKLOAD_EXITED".into(),
                assignment_id: "assignment".into(),
                attempt_id: "attempt".into(),
                ..Default::default()
            }
            .encode_to_vec(),
        );
        let bytes = original.snapshot().unwrap();
        let restored = Registry::restore(&bytes).unwrap();
        assert_eq!(
            restored.runtime_resume_tokens.get(&runtime_key).unwrap(),
            "runtime-token"
        );
        assert_eq!(restored.accepted_assignments.len(), 1);
        assert_eq!(restored.terminal_observations.len(), 1);
        assert!(restored.hosts.is_empty() && restored.runtimes.is_empty());
        assert_eq!(restored.snapshot().unwrap(), bytes);
        assert!(restored
            .check_node_epoch(&NodeKey {
                node_id: "host".into(),
                node_epoch: 2
            })
            .is_err());
        assert!(Registry::restore(b"{}").is_err());
    }
}
