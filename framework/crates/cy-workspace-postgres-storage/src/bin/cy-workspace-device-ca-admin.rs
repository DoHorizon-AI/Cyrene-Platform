//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-device-ca-admin.rs                                │
//! │  Module: cy_workspace_device_ca_admin                              │
//! │  Role: Apply CA schema or verify the local signer and current CRL.   │
//! │                                                                     │
//! │  模块职责：迁移设备 CA schema，或核验本机 signer 与当前签名 CRL。       │
//! └─────────────────────────────────────────────────────────────────────┘

use std::process::ExitCode;

use cy_workspace_postgres_storage::{PostgresRestrictedDeviceCa, RestrictedDeviceCaError};

/// Runs one explicit, non-secret CA administration action.
fn run() -> Result<(), RestrictedDeviceCaError> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(RestrictedDeviceCaError::Configuration);
    };
    if arguments.next().is_some() {
        return Err(RestrictedDeviceCaError::Configuration);
    }

    match command.to_str() {
        Some("migrate") => PostgresRestrictedDeviceCa::migrate_from_environment(),
        Some("check") => {
            let ca = PostgresRestrictedDeviceCa::connect_from_environment()?;
            ca.check_current_crl()
        }
        Some("--help") | Some("-h") => {
            println!("Usage: cy-workspace-device-ca-admin <migrate|check>");
            println!("Configure the CA operator/runtime environment variables separately.");
            Ok(())
        }
        _ => Err(RestrictedDeviceCaError::Configuration),
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
