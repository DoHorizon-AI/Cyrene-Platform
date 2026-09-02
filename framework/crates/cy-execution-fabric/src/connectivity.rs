//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connectivity.rs                                                 │
//! │  Module: cy_execution_fabric::connectivity                          │
//! │  Role: Replaceable outbound connectivity selection seam.           │
//! │                                                                     │
//! │  模块职责：提供不影响 Runtime 身份的可替换出站连接策略。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use cy_proto::core_v1::ConnectivityMode;

use crate::FabricContractError;

/// Resolved outbound route; addresses never become Runtime identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityRoute {
    pub mode: ConnectivityMode,
    pub control_endpoint: String,
    pub server_name: String,
    pub outbound_only: bool,
}

/// Provider port for relay, direct, mesh, or cloud-private connectivity.
pub trait ConnectivityProvider: Send + Sync {
    fn resolve(&self) -> Result<ConnectivityRoute, FabricContractError>;
}

/// MVP local route for a Control Plane reachable inside the same environment.
#[derive(Debug, Clone)]
pub struct LocalConnectivityProvider {
    route: ConnectivityRoute,
}

impl LocalConnectivityProvider {
    pub fn new(control_endpoint: impl Into<String>, server_name: impl Into<String>) -> Self {
        Self {
            route: ConnectivityRoute {
                mode: ConnectivityMode::Local,
                control_endpoint: control_endpoint.into(),
                server_name: server_name.into(),
                outbound_only: true,
            },
        }
    }
}

impl ConnectivityProvider for LocalConnectivityProvider {
    fn resolve(&self) -> Result<ConnectivityRoute, FabricContractError> {
        validate_route(&self.route)?;
        Ok(self.route.clone())
    }
}

/// Relay-first route; the Agent still opens the authenticated connection.
#[derive(Debug, Clone)]
pub struct RelayConnectivityProvider {
    route: ConnectivityRoute,
}

impl RelayConnectivityProvider {
    pub fn new(control_endpoint: impl Into<String>, server_name: impl Into<String>) -> Self {
        Self {
            route: ConnectivityRoute {
                mode: ConnectivityMode::Relay,
                control_endpoint: control_endpoint.into(),
                server_name: server_name.into(),
                outbound_only: true,
            },
        }
    }
}

impl ConnectivityProvider for RelayConnectivityProvider {
    fn resolve(&self) -> Result<ConnectivityRoute, FabricContractError> {
        validate_route(&self.route)?;
        Ok(self.route.clone())
    }
}

fn validate_route(route: &ConnectivityRoute) -> Result<(), FabricContractError> {
    if !matches!(
        route.mode,
        ConnectivityMode::Local | ConnectivityMode::Relay
    ) || !route.control_endpoint.starts_with("https://")
        || route.server_name.is_empty()
        || !route.outbound_only
    {
        return Err(FabricContractError {
            reason_code: "CONNECTIVITY_ROUTE_INVALID",
            message: "MVP connectivity requires LOCAL/RELAY outbound HTTPS with a server name"
                .to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_is_outbound_only_and_does_not_change_identity() {
        let route = RelayConnectivityProvider::new("https://relay.example", "relay.example")
            .resolve()
            .unwrap();
        assert_eq!(route.mode, ConnectivityMode::Relay);
        assert!(route.outbound_only);
    }
}
