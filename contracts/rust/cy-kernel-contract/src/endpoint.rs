// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/endpoint.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Endpoint 暴露与 EndpointGrant 授权模型。

use std::collections::BTreeMap;

use crate::{
    identity::Identity,
    lease::{Lease, LeaseState},
    resource::Capability,
    validation::{
        validate_capabilities, validate_namespaced_id, validate_properties, validate_text,
        validate_timestamp, ContractError, MAX_CONNECTION_REF_BYTES,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub identity: Identity,
    pub provider: Identity,
    pub owner: Identity,
    pub transport: String,
    pub schema_id: String,
    pub capabilities: Vec<Capability>,
    pub public_attributes: BTreeMap<String, String>,
    /// Opaque location consumed by the Product-side direct transport client.
    pub connection_ref: String,
    /// Secret-provider reference. The actual credential is never stored here.
    pub credential_ref: Option<String>,
}

impl Endpoint {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.provider.validate()?;
        self.owner.validate()?;
        validate_namespaced_id("endpoint transport", &self.transport)?;
        validate_namespaced_id("endpoint schema id", &self.schema_id)?;
        validate_capabilities(&self.capabilities)?;
        validate_properties(&self.public_attributes)?;
        validate_text(
            "endpoint connection reference",
            &self.connection_ref,
            MAX_CONNECTION_REF_BYTES,
        )?;
        if let Some(credential_ref) = &self.credential_ref {
            validate_namespaced_id("endpoint credential reference", credential_ref)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointGrant {
    pub identity: Identity,
    pub endpoint: Identity,
    pub grantee: Identity,
    pub lease: Identity,
    pub fence_token: u64,
    pub expires_at_unix_ms: u64,
}

impl EndpointGrant {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.identity.validate()?;
        self.endpoint.validate()?;
        self.grantee.validate()?;
        self.lease.validate()?;
        if self.fence_token == 0 {
            return Err(ContractError::new(
                "FENCE_TOKEN_INVALID",
                "endpoint grant fence token must be non-zero",
            ));
        }
        validate_timestamp("endpoint grant expiry", self.expires_at_unix_ms)
    }

    /// Endpoint data bypasses Kernel, but the grant remains authorized only
    /// while both the grant and its referenced Lease authority are current.
    pub fn authorizes(
        &self,
        endpoint: &Identity,
        grantee: &Identity,
        lease: &Lease,
        now_unix_ms: u64,
    ) -> bool {
        self.validate().is_ok()
            && lease.validate().is_ok()
            && &self.endpoint == endpoint
            && &self.grantee == grantee
            && self.lease == lease.identity
            && self.grantee == lease.holder
            && self.fence_token == lease.fence_token
            && lease.state == LeaseState::Active
            && lease
                .expires_at_unix_ms
                .is_some_and(|expires_at| now_unix_ms < expires_at)
            && now_unix_ms < self.expires_at_unix_ms
    }
}
