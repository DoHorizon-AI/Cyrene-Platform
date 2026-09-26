//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 product_authorization.rs                                        │
//! │  Module: cy_workspace_fabric::product_authorization                 │
//! │  Role: Fail-closed Product API authorization policy.                │
//! │                                                                     │
//! │  模块职责：为 Product API 定义默认拒绝的授权策略。                     │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! The caller must supply principal kind and roles obtained from trusted
//! authentication and Directory boundaries. This module does not parse caller
//! claims or establish identity, membership, or Workspace scope.

use std::collections::BTreeSet;

/// Directory-derived membership marker for a verified Workspace user.
pub(crate) const WORKSPACE_MEMBER_ROLE: &str = "workspace.member";

/// Directory role for the Catalyst `createDataset` command.
pub(crate) const CATALYST_CREATE_DATASET_ROLE: &str =
    "workspace.product.command.catalyst.create_dataset.v1";
/// Directory role for the Yield `start_run` command.
pub(crate) const YIELD_START_RUN_ROLE: &str =
    "workspace.product.command.yield.start_training_run.v1";
/// Directory role for the Reactor `create_model_import` command.
pub(crate) const REACTOR_CREATE_MODEL_IMPORT_ROLE: &str =
    "workspace.product.command.reactor.create_model_import.v1";
/// Directory role for the Exchange route-draft command.
pub(crate) const EXCHANGE_CREATE_ROUTE_DRAFT_ROLE: &str =
    "workspace.product.command.exchange.create_route_draft.v1";
/// Directory role for the Echo `createEvaluationSuite` command.
pub(crate) const ECHO_CREATE_EVALUATION_SUITE_ROLE: &str =
    "workspace.product.command.echo.create_evaluation_suite.v1";

/// Principal class established by a trusted server-side authentication boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductAuthorizationPrincipal {
    /// A user whose current Workspace membership was resolved by Directory.
    DirectoryUser,
    /// A Workspace device without a separately defined Product API grant.
    WorkspaceDevice,
    /// A workload that has not been attested as the Navigator Harness writer.
    UntrustedWorkload,
    /// A workload attested by a trusted server-side Navigator Harness handoff.
    ///
    /// The current authentication flow must not produce this value until that
    /// handoff validates workload identity, audience, organization, and Workspace.
    TrustedNavigatorHarnessWriter,
}

/// Product whose read projection appears in the Workspace operation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductReadOwner {
    Catalyst,
    Yield,
    Reactor,
    Exchange,
    Echo,
    Navigator,
}

/// Exact user command covered by a versioned Directory role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductCommand {
    CatalystCreateDataset,
    YieldStartRun,
    ReactorCreateModelImport,
    ExchangeCreateRouteDraft,
    EchoCreateEvaluationSuite,
}

impl ProductCommand {
    fn required_role(self) -> &'static str {
        match self {
            Self::CatalystCreateDataset => CATALYST_CREATE_DATASET_ROLE,
            Self::YieldStartRun => YIELD_START_RUN_ROLE,
            Self::ReactorCreateModelImport => REACTOR_CREATE_MODEL_IMPORT_ROLE,
            Self::ExchangeCreateRouteDraft => EXCHANGE_CREATE_ROUTE_DRAFT_ROLE,
            Self::EchoCreateEvaluationSuite => ECHO_CREATE_EVALUATION_SUITE_ROLE,
        }
    }
}

/// Closed operation class supplied by the Workspace Product operation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductAuthorizationOperation {
    /// A read operation for the specified Product owner.
    Read(ProductReadOwner),
    /// A user command with an exact Product operation role.
    Command(ProductCommand),
    /// Navigator Harness persistence `append_events`.
    NavigatorHarnessAppendEvents,
    /// Any operation that has not been mapped into this policy version.
    Unmapped,
}

/// Fail-closed reasons for denying a Product API operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProductAuthorizationDenied {
    /// No Directory-derived membership marker is present.
    MembershipRequired,
    /// The exact command role is absent from the Directory role set.
    CommandRoleRequired,
    /// The caller is not a trusted Navigator Harness service writer.
    TrustedNavigatorWriterRequired,
    /// The principal class has no Product API permission in this policy version.
    PrincipalNotAuthorized,
    /// The operation is not mapped to a permission in this policy version.
    UnmappedOperation,
}

/// Applies the Product authorization matrix to trusted authentication inputs.
///
/// Membership permits reads. A user command additionally requires its exact
/// Directory role. Navigator Harness append is reserved for the separately
/// attested service principal; a role string on a user never grants append.
pub(crate) fn authorize_product_operation(
    principal: ProductAuthorizationPrincipal,
    directory_roles: &BTreeSet<String>,
    operation: ProductAuthorizationOperation,
) -> Result<(), ProductAuthorizationDenied> {
    if operation == ProductAuthorizationOperation::Unmapped {
        return Err(ProductAuthorizationDenied::UnmappedOperation);
    }

    if principal == ProductAuthorizationPrincipal::TrustedNavigatorHarnessWriter {
        return if operation == ProductAuthorizationOperation::NavigatorHarnessAppendEvents {
            Ok(())
        } else {
            Err(ProductAuthorizationDenied::PrincipalNotAuthorized)
        };
    }

    if operation == ProductAuthorizationOperation::NavigatorHarnessAppendEvents {
        return Err(ProductAuthorizationDenied::TrustedNavigatorWriterRequired);
    }

    if principal != ProductAuthorizationPrincipal::DirectoryUser {
        return Err(ProductAuthorizationDenied::PrincipalNotAuthorized);
    }

    if !directory_roles.contains(WORKSPACE_MEMBER_ROLE) {
        return Err(ProductAuthorizationDenied::MembershipRequired);
    }

    match operation {
        ProductAuthorizationOperation::Read(_owner) => Ok(()),
        ProductAuthorizationOperation::Command(command) => {
            if directory_roles.contains(command.required_role()) {
                Ok(())
            } else {
                Err(ProductAuthorizationDenied::CommandRoleRequired)
            }
        }
        ProductAuthorizationOperation::NavigatorHarnessAppendEvents => {
            Err(ProductAuthorizationDenied::TrustedNavigatorWriterRequired)
        }
        ProductAuthorizationOperation::Unmapped => {
            Err(ProductAuthorizationDenied::UnmappedOperation)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ_OWNERS: [ProductReadOwner; 6] = [
        ProductReadOwner::Catalyst,
        ProductReadOwner::Yield,
        ProductReadOwner::Reactor,
        ProductReadOwner::Exchange,
        ProductReadOwner::Echo,
        ProductReadOwner::Navigator,
    ];

    const COMMANDS: [ProductCommand; 5] = [
        ProductCommand::CatalystCreateDataset,
        ProductCommand::YieldStartRun,
        ProductCommand::ReactorCreateModelImport,
        ProductCommand::ExchangeCreateRouteDraft,
        ProductCommand::EchoCreateEvaluationSuite,
    ];

    fn roles(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn member_roles() -> BTreeSet<String> {
        roles(&[WORKSPACE_MEMBER_ROLE])
    }

    #[test]
    fn directory_member_can_read_each_manifest_product_without_command_roles() {
        let roles = member_roles();

        for owner in READ_OWNERS {
            assert_eq!(
                authorize_product_operation(
                    ProductAuthorizationPrincipal::DirectoryUser,
                    &roles,
                    ProductAuthorizationOperation::Read(owner),
                ),
                Ok(())
            );
        }
    }

    #[test]
    fn membership_alone_never_grants_a_product_command() {
        let roles = member_roles();

        for command in COMMANDS {
            assert_eq!(
                authorize_product_operation(
                    ProductAuthorizationPrincipal::DirectoryUser,
                    &roles,
                    ProductAuthorizationOperation::Command(command),
                ),
                Err(ProductAuthorizationDenied::CommandRoleRequired)
            );
        }
    }

    #[test]
    fn each_command_role_grants_only_its_exact_command() {
        for allowed_command in COMMANDS {
            let roles = roles(&[WORKSPACE_MEMBER_ROLE, allowed_command.required_role()]);

            assert_eq!(
                authorize_product_operation(
                    ProductAuthorizationPrincipal::DirectoryUser,
                    &roles,
                    ProductAuthorizationOperation::Command(allowed_command),
                ),
                Ok(())
            );

            for other_command in COMMANDS {
                if other_command != allowed_command {
                    assert_eq!(
                        authorize_product_operation(
                            ProductAuthorizationPrincipal::DirectoryUser,
                            &roles,
                            ProductAuthorizationOperation::Command(other_command),
                        ),
                        Err(ProductAuthorizationDenied::CommandRoleRequired)
                    );
                }
            }
        }
    }

    #[test]
    fn command_role_without_directory_membership_is_denied() {
        let roles = roles(&[CATALYST_CREATE_DATASET_ROLE]);

        assert_eq!(
            authorize_product_operation(
                ProductAuthorizationPrincipal::DirectoryUser,
                &roles,
                ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset),
            ),
            Err(ProductAuthorizationDenied::MembershipRequired)
        );
    }

    #[test]
    fn empty_or_unknown_roles_fail_closed_for_commands() {
        assert_eq!(
            authorize_product_operation(
                ProductAuthorizationPrincipal::DirectoryUser,
                &BTreeSet::new(),
                ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset),
            ),
            Err(ProductAuthorizationDenied::MembershipRequired)
        );

        let unknown_roles = roles(&[WORKSPACE_MEMBER_ROLE, "workspace.admin"]);
        assert_eq!(
            authorize_product_operation(
                ProductAuthorizationPrincipal::DirectoryUser,
                &unknown_roles,
                ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset),
            ),
            Err(ProductAuthorizationDenied::CommandRoleRequired)
        );
    }

    #[test]
    fn a_directory_role_cannot_turn_a_user_into_the_navigator_service_writer() {
        let roles = roles(&[
            WORKSPACE_MEMBER_ROLE,
            "navigator.service-writer",
            CATALYST_CREATE_DATASET_ROLE,
        ]);

        assert_eq!(
            authorize_product_operation(
                ProductAuthorizationPrincipal::DirectoryUser,
                &roles,
                ProductAuthorizationOperation::NavigatorHarnessAppendEvents,
            ),
            Err(ProductAuthorizationDenied::TrustedNavigatorWriterRequired)
        );
    }

    #[test]
    fn only_the_attested_navigator_writer_can_append_and_it_has_no_other_grants() {
        let roles = BTreeSet::new();
        let principal = ProductAuthorizationPrincipal::TrustedNavigatorHarnessWriter;

        assert_eq!(
            authorize_product_operation(
                principal,
                &roles,
                ProductAuthorizationOperation::NavigatorHarnessAppendEvents,
            ),
            Ok(())
        );
        assert_eq!(
            authorize_product_operation(
                principal,
                &roles,
                ProductAuthorizationOperation::Read(ProductReadOwner::Navigator),
            ),
            Err(ProductAuthorizationDenied::PrincipalNotAuthorized)
        );
        assert_eq!(
            authorize_product_operation(
                principal,
                &roles,
                ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset),
            ),
            Err(ProductAuthorizationDenied::PrincipalNotAuthorized)
        );
    }

    #[test]
    fn untrusted_workloads_and_devices_have_no_product_grants() {
        let roles = roles(&[WORKSPACE_MEMBER_ROLE, CATALYST_CREATE_DATASET_ROLE]);

        for principal in [
            ProductAuthorizationPrincipal::UntrustedWorkload,
            ProductAuthorizationPrincipal::WorkspaceDevice,
        ] {
            assert_eq!(
                authorize_product_operation(
                    principal,
                    &roles,
                    ProductAuthorizationOperation::Read(ProductReadOwner::Catalyst),
                ),
                Err(ProductAuthorizationDenied::PrincipalNotAuthorized)
            );
            assert_eq!(
                authorize_product_operation(
                    principal,
                    &roles,
                    ProductAuthorizationOperation::Command(ProductCommand::CatalystCreateDataset),
                ),
                Err(ProductAuthorizationDenied::PrincipalNotAuthorized)
            );
        }
    }

    #[test]
    fn unmapped_operations_are_denied_even_for_the_service_writer() {
        assert_eq!(
            authorize_product_operation(
                ProductAuthorizationPrincipal::TrustedNavigatorHarnessWriter,
                &BTreeSet::new(),
                ProductAuthorizationOperation::Unmapped,
            ),
            Err(ProductAuthorizationDenied::UnmappedOperation)
        );
    }
}
