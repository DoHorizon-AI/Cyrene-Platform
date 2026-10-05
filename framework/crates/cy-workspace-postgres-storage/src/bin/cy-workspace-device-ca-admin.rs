//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-device-ca-admin.rs                                │
//! │  Module: cy_workspace_device_ca_admin                              │
//! │  Role: Apply CA schema or verify the local signer and current CRL.   │
//! │                                                                     │
//! │  模块职责：迁移设备 CA schema，或核验本机 signer 与当前签名 CRL。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::process::ExitCode;

use cy_workspace_control_plane::device_registry::{WorkspaceDeviceKey, WorkspaceDeviceRegistry};
use cy_workspace_postgres_storage::PostgresWorkspaceDeviceRegistry;
use cy_workspace_postgres_storage::{PostgresRestrictedDeviceCa, RestrictedDeviceCaError};

/// Runs one explicit, non-secret CA administration action.
fn run() -> Result<(), RestrictedDeviceCaError> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| RestrictedDeviceCaError::Configuration)
        })
        .collect::<Result<Vec<_>, _>>()?;
    match arguments.as_slice() {
        [command] if command == "migrate" => PostgresRestrictedDeviceCa::migrate_from_environment(),
        [command] if command == "check" => {
            let ca = PostgresRestrictedDeviceCa::connect_from_environment()?;
            ca.check_current_crl()
        }
        [command] if command == "--help" || command == "-h" => {
            println!(
                "Usage: cy-workspace-device-ca-admin <migrate|check|revoke ORG WORKSPACE DEVICE>"
            );
            println!("Configure the CA operator/runtime environment variables separately.");
            Ok(())
        }
        [command, organization_id, workspace_id, device_id] if command == "revoke" => {
            let key = WorkspaceDeviceKey {
                organization_id: validate_device_key_part(organization_id)?,
                workspace_id: validate_device_key_part(workspace_id)?,
                device_id: validate_device_key_part(device_id)?,
            };

            // Commit the terminal Registry state before opening the CA signer. A later CA/CRL
            // failure leaves the device revoked and the same command safely retryable.
            let registry = PostgresWorkspaceDeviceRegistry::connect_from_environment()
                .map_err(|_| RestrictedDeviceCaError::Unavailable)?;
            if registry.revoke_device(&key).is_err()
                && !registry
                    .is_device_terminal(&key)
                    .map_err(|_| RestrictedDeviceCaError::Unavailable)?
            {
                return Err(RestrictedDeviceCaError::Unavailable);
            }
            drop(registry);

            let ca = PostgresRestrictedDeviceCa::connect_from_environment()?;
            ca.retire_device_certificates(&key)
        }
        _ => Err(RestrictedDeviceCaError::Configuration),
    }
}

fn validate_device_key_part(value: &str) -> Result<String, RestrictedDeviceCaError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(RestrictedDeviceCaError::Configuration);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::validate_device_key_part;
    use cy_workspace_postgres_storage::RestrictedDeviceCaError;

    #[test]
    fn revoke_key_parts_must_match_registry_identity_constraints() {
        assert_eq!(validate_device_key_part("device-01").unwrap(), "device-01");
        assert_eq!(
            validate_device_key_part(" device-01"),
            Err(RestrictedDeviceCaError::Configuration)
        );
        assert_eq!(
            validate_device_key_part("device\n01"),
            Err(RestrictedDeviceCaError::Configuration)
        );
        assert_eq!(
            validate_device_key_part(&"x".repeat(257)),
            Err(RestrictedDeviceCaError::Configuration)
        );
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("Workspace device CA administration failed; check private configuration and service logs.");
            ExitCode::FAILURE
        }
    }
}
