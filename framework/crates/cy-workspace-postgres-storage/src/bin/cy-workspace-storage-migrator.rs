//! Apply PostgreSQL Workspace schemas using separately provisioned migration URLs.
//!
//! This binary never accepts a database URL as an argument. Each adapter reads its dedicated
//! `*_MIGRATION_DATABASE_URL` environment variable and rejects application credentials at the
//! database boundary.

use std::error::Error;
use std::process::ExitCode;

use cy_workspace_postgres_storage::{
    device_authorization_postgres::PostgresDeviceAuthorizationStore,
    device_registry_postgres::PostgresWorkspaceDeviceRegistry,
    durable_directory::DirectoryOperatorProvisioner,
    restricted_device_ca::PostgresRestrictedDeviceCa,
    webauthn_http_binding_postgres::PostgresWebAuthnHttpSessionBindingStore,
    webauthn_postgres_store::PostgresWebAuthnCredentialStore,
};

const COMPONENTS: &[&str] = &[
    "directory",
    "device-authorization",
    "device-registry",
    "webauthn",
    "webauthn-http-binding",
    "device-ca",
];

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Workspace schema migration failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Err(usage_error().into());
    };
    if arguments.next().is_some() {
        return Err(usage_error().into());
    }

    if command == "migrate-all" {
        for component in COMPONENTS {
            migrate(component).await?;
            println!("migrated {component}");
        }
        return Ok(());
    }
    if command == "help" || command == "--help" || command == "-h" {
        println!("{USAGE}");
        return Ok(());
    }
    if let Some(component) = command.strip_prefix("migrate-") {
        if COMPONENTS.contains(&component) {
            migrate(component).await?;
            println!("migrated {component}");
            return Ok(());
        }
    }
    Err(usage_error().into())
}

async fn migrate(component: &str) -> Result<(), Box<dyn Error>> {
    match component {
        "directory" => DirectoryOperatorProvisioner::migrate_from_environment().await?,
        "device-authorization" => {
            tokio::task::spawn_blocking(PostgresDeviceAuthorizationStore::migrate_from_environment)
                .await??;
        }
        "device-registry" => {
            tokio::task::spawn_blocking(PostgresWorkspaceDeviceRegistry::migrate_from_environment)
                .await??;
        }
        "webauthn" => {
            tokio::task::spawn_blocking(PostgresWebAuthnCredentialStore::migrate_from_environment)
                .await??;
        }
        "webauthn-http-binding" => {
            PostgresWebAuthnHttpSessionBindingStore::migrate_from_environment().await?;
        }
        "device-ca" => {
            tokio::task::spawn_blocking(PostgresRestrictedDeviceCa::migrate_from_environment)
                .await??;
        }
        _ => return Err(usage_error().into()),
    }
    Ok(())
}

fn usage_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, USAGE)
}

const USAGE: &str = "Usage: cy-workspace-storage-migrator <migrate-all|migrate-component|help>\nComponents: directory, device-authorization, device-registry, webauthn, webauthn-http-binding, device-ca\nMigration URLs are read only from the component-specific *_MIGRATION_DATABASE_URL environment variables.";
