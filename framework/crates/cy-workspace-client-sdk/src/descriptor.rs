//! Workspace connection descriptor validation shared by lightweight clients.
//!
//! The validator checks only public transport facts. Membership and trust decisions remain
//! server-side concerns and must never be inferred from a valid descriptor alone.

use std::collections::BTreeSet;

use cy_proto::core_v1::ConnectivityMode;
use cy_proto::workspace_v1::WorkspaceConnectionDescriptor;
use thiserror::Error;

/// Stable failure category for malformed or expired Workspace descriptors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("workspace connection descriptor is invalid: {0}")]
pub struct WorkspaceDescriptorValidationError(&'static str);

impl WorkspaceDescriptorValidationError {
    fn new(reason: &'static str) -> Self {
        Self(reason)
    }
}

/// Validate descriptor shape, transport candidates, and expiry.
pub fn validate_workspace_descriptor(
    descriptor: &WorkspaceConnectionDescriptor,
    now_unix_ms: u64,
) -> Result<(), WorkspaceDescriptorValidationError> {
    if descriptor.descriptor_version != "cyrene.workspace.connection.v1"
        || descriptor.workspace_id.is_empty()
        || descriptor.organization_id.is_empty()
        || descriptor.display_name.is_empty()
        || descriptor.candidates.is_empty()
    {
        return Err(WorkspaceDescriptorValidationError::new(
            "version, identities, display name, and candidates are required",
        ));
    }
    let expires_at = descriptor
        .expires_at
        .as_ref()
        .map(timestamp_ms)
        .unwrap_or_default();
    if expires_at <= now_unix_ms {
        return Err(WorkspaceDescriptorValidationError::new(
            "descriptor is expired",
        ));
    }

    let mut candidates = BTreeSet::new();
    let mut direct_priority = None;
    let mut relay_priority = None;
    for candidate in &descriptor.candidates {
        let mode = ConnectivityMode::try_from(candidate.mode)
            .map_err(|_| WorkspaceDescriptorValidationError::new("unknown connectivity mode"))?;
        if mode == ConnectivityMode::Unspecified
            || candidate.provider_id.is_empty()
            || candidate.connection_uri.is_empty()
            || candidate.server_name.is_empty()
        {
            return Err(WorkspaceDescriptorValidationError::new(
                "candidate mode, provider, URI, and server name are required",
            ));
        }
        if matches!(mode, ConnectivityMode::LanDirect | ConnectivityMode::Relay)
            && !candidate.connection_uri.starts_with("https://")
        {
            return Err(WorkspaceDescriptorValidationError::new(
                "LAN_DIRECT and RELAY candidates require HTTPS",
            ));
        }
        if !candidates.insert((
            candidate.mode,
            candidate.provider_id.clone(),
            candidate.connection_uri.clone(),
        )) {
            return Err(WorkspaceDescriptorValidationError::new(
                "duplicate connectivity candidate",
            ));
        }
        match mode {
            ConnectivityMode::LanDirect => {
                direct_priority =
                    Some(direct_priority.map_or(candidate.priority, |priority: u32| {
                        priority.min(candidate.priority)
                    }));
            }
            ConnectivityMode::Relay => {
                relay_priority =
                    Some(relay_priority.map_or(candidate.priority, |priority: u32| {
                        priority.min(candidate.priority)
                    }));
            }
            _ => {}
        }
    }
    if direct_priority
        .zip(relay_priority)
        .is_some_and(|(direct, relay)| direct >= relay)
    {
        return Err(WorkspaceDescriptorValidationError::new(
            "LAN_DIRECT must precede RELAY",
        ));
    }
    Ok(())
}

fn timestamp_ms(value: &prost_types::Timestamp) -> u64 {
    u64::try_from(value.seconds)
        .unwrap_or_default()
        .saturating_mul(1000)
        .saturating_add(u64::try_from(value.nanos).unwrap_or_default() / 1_000_000)
}

#[cfg(test)]
mod tests {
    use cy_proto::core_v1::ConnectivityMode;
    use cy_proto::workspace_v1::WorkspaceConnectionCandidate;

    use super::*;

    fn descriptor() -> WorkspaceConnectionDescriptor {
        WorkspaceConnectionDescriptor {
            descriptor_version: "cyrene.workspace.connection.v1".to_string(),
            workspace_id: "workspace-1".to_string(),
            organization_id: "organization-1".to_string(),
            display_name: "Workspace One".to_string(),
            candidates: vec![
                WorkspaceConnectionCandidate {
                    mode: ConnectivityMode::LanDirect as i32,
                    provider_id: "tailscale".to_string(),
                    connection_uri: "https://100.64.0.1:443".to_string(),
                    server_name: "workspace.internal".to_string(),
                    priority: 1,
                    routing_hint: Vec::new(),
                },
                WorkspaceConnectionCandidate {
                    mode: ConnectivityMode::Relay as i32,
                    provider_id: "relay".to_string(),
                    connection_uri: "https://relay.example".to_string(),
                    server_name: "relay.internal".to_string(),
                    priority: 2,
                    routing_hint: Vec::new(),
                },
            ],
            expires_at: Some(prost_types::Timestamp {
                seconds: 2,
                nanos: 0,
            }),
        }
    }

    #[test]
    fn accepts_supported_ordered_descriptor() {
        assert!(validate_workspace_descriptor(&descriptor(), 1).is_ok());
    }

    #[test]
    fn rejects_relay_before_direct_and_expired_descriptors() {
        let mut invalid = descriptor();
        invalid.candidates[0].priority = 3;
        invalid.candidates[1].priority = 2;
        assert!(validate_workspace_descriptor(&invalid, 1).is_err());
        assert!(validate_workspace_descriptor(&descriptor(), 2_000).is_err());
    }
}
