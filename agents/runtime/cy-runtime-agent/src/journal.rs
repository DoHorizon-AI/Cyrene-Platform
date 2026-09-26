//! Durable single-generation admission and terminal evidence, never process adoption.
use cy_proto::core_v1::{
    RuntimeAssignment, RuntimeObservation, RuntimeObservedState, TerminationClassification,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config::StateDirectory, RuntimeAgentConfig, RuntimeAgentError};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Admission {
    pub id: String,
    #[serde(default)]
    pub attempt_id: String,
    pub fingerprint: String,
    pub accepted: bool,
    pub rejected: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    binding: String,
    admission: Option<Admission>,
    terminal: Option<Vec<u8>>,
}

pub(crate) struct Journal {
    pub directory: StateDirectory,
    name: String,
    snapshot: Snapshot,
}

impl Journal {
    pub fn open(config: &RuntimeAgentConfig) -> Result<Self, RuntimeAgentError> {
        let directory = config.prepare_state_directory()?;
        let name = config
            .resume_token_state_name()
            .replace("runtime-resume-token-", "runtime-execution-");
        let binding = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &config.runtime.id,
                    config.runtime.generation,
                    &config.node.node_id,
                    config.node.node_epoch,
                    &config.organization_id,
                    &config.workspace_id,
                    &config.control_plane_endpoint,
                    &config.workload,
                ))
                .map_err(state_error)?
            )
        );
        let snapshot = match directory.read_record(&name)? {
            Some(bytes) => {
                let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(state_error)?;
                if snapshot.version != 1 || snapshot.binding != binding {
                    return Err(state_error(
                        "execution journal identity or launch configuration mismatch",
                    ));
                }
                if snapshot.admission.as_ref().is_some_and(|a| {
                    a.id.is_empty()
                        || a.attempt_id.is_empty()
                        || a.fingerprint.len() != 64
                        || (a.accepted && a.rejected)
                }) {
                    return Err(state_error("execution journal admission is invalid"));
                }
                if let Some(bytes) = &snapshot.terminal {
                    let value =
                        RuntimeObservation::decode(bytes.as_slice()).map_err(state_error)?;
                    let admission = snapshot.admission.as_ref().ok_or_else(|| {
                        state_error("execution journal terminal has no admission")
                    })?;
                    if !admission.accepted
                        || admission.rejected
                        || value.assignment_id != admission.id
                        || value.attempt_id != admission.attempt_id
                        || value
                            .runtime
                            .as_ref()
                            .and_then(|r| r.identity.as_ref())
                            .is_none_or(|r| {
                                r.id != config.runtime.id
                                    || r.generation != config.runtime.generation
                            })
                        || !matches!(
                            RuntimeObservedState::try_from(value.observed_state),
                            Ok(RuntimeObservedState::Stopped
                                | RuntimeObservedState::Failed
                                | RuntimeObservedState::Lost)
                        )
                        || TerminationClassification::try_from(value.termination).is_err()
                        || value.termination == TerminationClassification::Unspecified as i32
                    {
                        return Err(state_error(
                            "execution journal terminal identity is invalid",
                        ));
                    }
                }
                snapshot
            }
            None => {
                // An older Agent may already have executed a task. An enrollment
                // token alone cannot prove this generation has never started.
                if directory
                    .read_record(&config.resume_token_state_name())?
                    .is_some()
                {
                    return Err(state_error(
                        "legacy Runtime state has no execution journal; reconcile before reuse",
                    ));
                }
                Snapshot {
                    version: 1,
                    binding,
                    admission: None,
                    terminal: None,
                }
            }
        };
        let journal = Self {
            directory,
            name,
            snapshot,
        };
        journal.save()?;
        Ok(journal)
    }

    pub fn admission(&self) -> Option<&Admission> {
        self.snapshot.admission.as_ref()
    }
    pub fn terminal(&self) -> Result<Option<RuntimeObservation>, RuntimeAgentError> {
        self.snapshot
            .terminal
            .as_ref()
            .map(|bytes| RuntimeObservation::decode(bytes.as_slice()).map_err(state_error))
            .transpose()
    }
    pub fn fingerprint(assignment: &RuntimeAssignment) -> String {
        format!("{:x}", Sha256::digest(assignment.encode_to_vec()))
    }
    pub fn begin(&mut self, assignment: &RuntimeAssignment) -> Result<(), RuntimeAgentError> {
        if self.snapshot.admission.is_some() || self.snapshot.terminal.is_some() {
            return Err(state_error(
                "Runtime generation already has execution evidence",
            ));
        }
        self.snapshot.admission = Some(Admission {
            id: assignment.assignment_id.clone(),
            attempt_id: assignment.attempt_id.clone(),
            fingerprint: Self::fingerprint(assignment),
            accepted: false,
            rejected: false,
        });
        self.save()
    }
    pub fn mark_started(&mut self, accepted: bool) -> Result<(), RuntimeAgentError> {
        let admission = self
            .snapshot
            .admission
            .as_mut()
            .ok_or_else(|| state_error("missing spawn intent"))?;
        admission.accepted = accepted;
        admission.rejected = !accepted;
        self.save()
    }
    pub fn finish(&mut self, observation: &RuntimeObservation) -> Result<(), RuntimeAgentError> {
        if self.snapshot.terminal.is_some() {
            return Err(state_error("terminal evidence is immutable"));
        }
        self.snapshot.terminal = Some(observation.encode_to_vec());
        self.save()
    }
    fn save(&self) -> Result<(), RuntimeAgentError> {
        self.directory.write_record(
            &self.name,
            &serde_json::to_vec(&self.snapshot).map_err(state_error)?,
        )
    }
}

fn state_error(error: impl std::fmt::Display) -> RuntimeAgentError {
    RuntimeAgentError::State(error.to_string())
}
