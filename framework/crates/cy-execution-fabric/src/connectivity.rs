//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 connectivity.rs                                                 │
//! │  Module: cy_execution_fabric::connectivity                          │
//! │  Role: Replaceable outbound connectivity selection seam.           │
//! │                                                                     │
//! │  模块职责：提供不影响 Runtime 身份的可替换出站连接策略。                 │
//! └─────────────────────────────────────────────────────────────────────┘

use crate::FabricContractError;

/// Resolved outbound route; addresses never become Runtime identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectivityRoute {
    pub control_endpoint: String,
    pub server_name: String,
}

/// Provider port for relay, direct, mesh, or cloud-private connectivity.
pub trait ConnectivityProvider: Send + Sync {
    fn resolve(&self) -> Result<ConnectivityRoute, FabricContractError>;
}

/// MVP direct outbound TLS provider.
#[derive(Debug, Clone)]
pub struct DirectConnectivityProvider {
    route: ConnectivityRoute,
}

impl DirectConnectivityProvider {
    pub fn new(control_endpoint: impl Into<String>, server_name: impl Into<String>) -> Self {
        Self {
            route: ConnectivityRoute {
                control_endpoint: control_endpoint.into(),
                server_name: server_name.into(),
            },
        }
    }
}

impl ConnectivityProvider for DirectConnectivityProvider {
    fn resolve(&self) -> Result<ConnectivityRoute, FabricContractError> {
        if !self.route.control_endpoint.starts_with("https://") || self.route.server_name.is_empty()
        {
            return Err(FabricContractError {
                reason_code: "CONNECTIVITY_ROUTE_INVALID",
                message: "direct connectivity requires an HTTPS endpoint and server name"
                    .to_string(),
            });
        }
        Ok(self.route.clone())
    }
}
