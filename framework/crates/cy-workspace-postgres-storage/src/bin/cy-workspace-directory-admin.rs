//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 cy-workspace-directory-admin.rs                                 │
//! │  Module: cy_workspace_directory_admin                              │
//! │  Role: Restricted local PostgreSQL Directory provisioning CLI.      │
//! │                                                                     │
//! │  模块职责：受限本地 PostgreSQL Directory 配置工具。                    │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;

use cy_proto::workspace_v1::{UserIdentityRef, WorkspaceConnectionDescriptor};
use cy_workspace_postgres_storage::{
    DirectoryMutation, DirectoryOperatorProvisioner, DurableDirectoryError,
};
use prost::Message;

const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), CliError> {
    let mut arguments = std::env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Err(CliError::Usage);
    };
    if command == "migrate" {
        if arguments.next().is_some() {
            return Err(CliError::Usage);
        }
        DirectoryOperatorProvisioner::migrate_from_environment().await?;
        println!("workspace-directory migrations applied");
        return Ok(());
    }

    let options = parse_options(arguments)?;
    let operator = DirectoryOperatorProvisioner::connect_from_environment().await?;
    let mutation = match command.as_str() {
        "grant-membership" => {
            require_options(
                &options,
                &[
                    "issuer",
                    "subject",
                    "organization",
                    "workspace",
                    "reason",
                    "role",
                ],
            )?;
            operator
                .grant_membership(
                    &user(&options)?,
                    required(&options, "organization")?,
                    required(&options, "workspace")?,
                    &repeated(&options, "role"),
                    required(&options, "reason")?,
                )
                .await?
        }
        "revoke-membership" => {
            require_options(
                &options,
                &["issuer", "subject", "organization", "workspace", "reason"],
            )?;
            operator
                .revoke_membership(
                    &user(&options)?,
                    required(&options, "organization")?,
                    required(&options, "workspace")?,
                    required(&options, "reason")?,
                )
                .await?
        }
        "grant-role" => {
            require_options(
                &options,
                &[
                    "issuer",
                    "subject",
                    "organization",
                    "workspace",
                    "reason",
                    "role",
                ],
            )?;
            operator
                .grant_role(
                    &user(&options)?,
                    required(&options, "organization")?,
                    required(&options, "workspace")?,
                    required(&options, "role")?,
                    required(&options, "reason")?,
                )
                .await?
        }
        "revoke-role" => {
            require_options(
                &options,
                &[
                    "issuer",
                    "subject",
                    "organization",
                    "workspace",
                    "reason",
                    "role",
                ],
            )?;
            operator
                .revoke_role(
                    &user(&options)?,
                    required(&options, "organization")?,
                    required(&options, "workspace")?,
                    required(&options, "role")?,
                    required(&options, "reason")?,
                )
                .await?
        }
        "publish-descriptor" => {
            require_options(&options, &["file", "reason"])?;
            let descriptor = read_descriptor(required(&options, "file")?)?;
            operator
                .publish_descriptor(&descriptor, required(&options, "reason")?)
                .await?
        }
        "revoke-descriptor" => {
            require_options(&options, &["organization", "workspace", "reason"])?;
            operator
                .revoke_descriptor(
                    required(&options, "organization")?,
                    required(&options, "workspace")?,
                    required(&options, "reason")?,
                )
                .await?
        }
        _ => return Err(CliError::Usage),
    };

    match mutation {
        DirectoryMutation::Changed => println!("workspace-directory change committed"),
        DirectoryMutation::Unchanged => println!("workspace-directory already in requested state"),
    }
    Ok(())
}

fn parse_options(
    arguments: impl Iterator<Item = String>,
) -> Result<BTreeMap<String, Vec<String>>, CliError> {
    let mut options = BTreeMap::<String, Vec<String>>::new();
    let mut arguments = arguments;
    while let Some(name) = arguments.next() {
        let Some(name) = name.strip_prefix("--") else {
            return Err(CliError::Usage);
        };
        if name.is_empty() {
            return Err(CliError::Usage);
        }
        let value = arguments.next().ok_or(CliError::Usage)?;
        if value.starts_with("--") || value.is_empty() {
            return Err(CliError::Usage);
        }
        let values = options.entry(name.to_string()).or_default();
        if !values.is_empty() && name != "role" {
            return Err(CliError::Usage);
        }
        values.push(value);
    }
    Ok(options)
}

fn require_options(
    options: &BTreeMap<String, Vec<String>>,
    allowed: &[&str],
) -> Result<(), CliError> {
    if options
        .keys()
        .any(|key| !allowed.iter().any(|allowed_key| key == allowed_key))
    {
        return Err(CliError::Usage);
    }
    for key in allowed.iter().filter(|key| **key != "role") {
        if options.get(*key).is_some_and(|values| values.len() != 1) {
            return Err(CliError::Usage);
        }
    }
    if options.get("role").is_some_and(Vec::is_empty) {
        return Err(CliError::Usage);
    }
    Ok(())
}

fn required<'a>(
    options: &'a BTreeMap<String, Vec<String>>,
    key: &str,
) -> Result<&'a str, CliError> {
    options
        .get(key)
        .and_then(|values| (values.len() == 1).then(|| values[0].as_str()))
        .ok_or(CliError::Usage)
}

fn repeated(options: &BTreeMap<String, Vec<String>>, key: &str) -> BTreeSet<String> {
    options.get(key).into_iter().flatten().cloned().collect()
}

fn user(options: &BTreeMap<String, Vec<String>>) -> Result<UserIdentityRef, CliError> {
    Ok(UserIdentityRef {
        issuer: required(options, "issuer")?.to_string(),
        subject: required(options, "subject")?.to_string(),
    })
}

fn read_descriptor(path: &str) -> Result<WorkspaceConnectionDescriptor, CliError> {
    let bytes = fs::read(PathBuf::from(path)).map_err(|_| CliError::InputFile)?;
    if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(CliError::InvalidDescriptor);
    }
    WorkspaceConnectionDescriptor::decode(bytes.as_slice()).map_err(|_| CliError::InvalidDescriptor)
}

#[derive(Debug, PartialEq, Eq)]
enum CliError {
    Usage,
    InputFile,
    InvalidDescriptor,
    Store(DurableDirectoryError),
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage => formatter.write_str(USAGE),
            Self::InputFile => formatter.write_str("WORKSPACE_DIRECTORY_INPUT_FILE_INVALID"),
            Self::InvalidDescriptor => {
                formatter.write_str("WORKSPACE_DIRECTORY_DESCRIPTOR_INVALID")
            }
            Self::Store(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for CliError {}

impl From<DurableDirectoryError> for CliError {
    fn from(error: DurableDirectoryError) -> Self {
        Self::Store(error)
    }
}

const USAGE: &str = "Usage:\n  cy-workspace-directory-admin migrate\n  cy-workspace-directory-admin grant-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID [--role ROLE]... --reason TEXT\n  cy-workspace-directory-admin revoke-membership --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --reason TEXT\n  cy-workspace-directory-admin grant-role --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --role ROLE --reason TEXT\n  cy-workspace-directory-admin revoke-role --issuer ISSUER --subject SUBJECT --organization ORG --workspace ID --role ROLE --reason TEXT\n  cy-workspace-directory-admin publish-descriptor --file PROTO_BIN --reason TEXT\n  cy-workspace-directory-admin revoke-descriptor --organization ORG --workspace ID --reason TEXT\n\nThe operator actor and database URLs are read from trusted environment configuration, never from a client request.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rejects_client_supplied_operator_identity_and_unknown_options() {
        let options = parse_options(
            ["--operator", "client", "--reason", "ticket"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert!(require_options(&options, &["reason"]).is_err());
    }

    #[test]
    fn cli_allows_repeated_roles_only_for_membership_grants() {
        let options = parse_options(
            [
                "--issuer",
                "iss",
                "--subject",
                "sub",
                "--role",
                "one",
                "--role",
                "two",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert_eq!(
            repeated(&options, "role"),
            BTreeSet::from(["one".into(), "two".into()])
        );
        assert!(require_options(&options, &["issuer", "subject", "role"]).is_ok());
        assert_eq!(
            parse_options(
                ["--issuer", "one", "--issuer", "two"]
                    .into_iter()
                    .map(str::to_string)
            )
            .unwrap_err(),
            CliError::Usage
        );
    }
}
