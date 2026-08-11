//! 提供商全量快照与 v1 SemanticAction 枚举。

use std::collections::BTreeSet;

use crate::{
    endpoint::Endpoint,
    identity::Identity,
    resource::Resource,
    validation::{
        validate_timestamp, ContractError, MAX_ENDPOINTS_PER_SNAPSHOT, MAX_RESOURCES_PER_SNAPSHOT,
        MAX_WORKERS_PER_SNAPSHOT,
    },
    worker::Worker,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub provider: Identity,
    pub snapshot_generation: u64,
    pub resources: Vec<Resource>,
    pub workers: Vec<Worker>,
    pub endpoints: Vec<Endpoint>,
    pub sampled_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

impl ProviderSnapshot {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.provider.validate()?;
        if self.snapshot_generation == 0 {
            return Err(ContractError::new(
                "SNAPSHOT_GENERATION_INVALID",
                "provider snapshot generation must be non-zero",
            ));
        }
        if self.expires_at_unix_ms <= self.sampled_at_unix_ms {
            return Err(ContractError::new(
                "SNAPSHOT_EXPIRY_INVALID",
                "provider snapshot expiry must be after sampling time",
            ));
        }
        validate_timestamp("provider snapshot sample time", self.sampled_at_unix_ms)?;
        validate_timestamp("provider snapshot expiry", self.expires_at_unix_ms)?;
        if self.resources.len() > MAX_RESOURCES_PER_SNAPSHOT {
            return Err(ContractError::new(
                "RESOURCE_LIMIT_EXCEEDED",
                "provider snapshot has too many resources",
            ));
        }
        let mut resource_ids = BTreeSet::new();
        for resource in &self.resources {
            resource.validate()?;
            if resource.provider != self.provider {
                return Err(ContractError::new(
                    "RESOURCE_PROVIDER_MISMATCH",
                    "resource provider does not match snapshot provider",
                ));
            }
            if !resource_ids.insert(resource.identity.id.as_str()) {
                return Err(ContractError::new(
                    "RESOURCE_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate resource identity",
                ));
            }
        }
        if self.workers.len() > MAX_WORKERS_PER_SNAPSHOT {
            return Err(ContractError::new(
                "WORKER_LIMIT_EXCEEDED",
                "provider snapshot has too many workers",
            ));
        }
        let mut worker_ids = BTreeSet::new();
        for worker in &self.workers {
            worker.validate()?;
            if worker.provider != self.provider {
                return Err(ContractError::new(
                    "WORKER_PROVIDER_MISMATCH",
                    "worker provider does not match snapshot provider",
                ));
            }
            if !worker_ids.insert(worker.identity.id.as_str()) {
                return Err(ContractError::new(
                    "WORKER_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate worker identity",
                ));
            }
        }
        if self.endpoints.len() > MAX_ENDPOINTS_PER_SNAPSHOT {
            return Err(ContractError::new(
                "ENDPOINT_LIMIT_EXCEEDED",
                "provider snapshot has too many endpoints",
            ));
        }
        let mut endpoint_ids = BTreeSet::new();
        for endpoint in &self.endpoints {
            endpoint.validate()?;
            if endpoint.provider != self.provider {
                return Err(ContractError::new(
                    "ENDPOINT_PROVIDER_MISMATCH",
                    "endpoint provider does not match snapshot provider",
                ));
            }
            if !endpoint_ids.insert(endpoint.identity.id.as_str()) {
                return Err(ContractError::new(
                    "ENDPOINT_IDENTITY_DUPLICATE",
                    "provider snapshot contains a duplicate endpoint identity",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticAction {
    Negotiate,
    RegisterProvider,
    PublishInventory,
    ReconcileProvider,
    AcquireLease,
    RenewLease,
    ReleaseLease,
    StartWorker,
    HeartbeatWorker,
    StopWorker,
    CreateOperation,
    ReportOperation,
    CancelOperation,
    PublishEndpoint,
    AuthorizeEndpoint,
    RevokeEndpoint,
    SubscribeEvents,
}

pub const V1_ACTIONS: &[SemanticAction] = &[
    SemanticAction::Negotiate,
    SemanticAction::RegisterProvider,
    SemanticAction::PublishInventory,
    SemanticAction::ReconcileProvider,
    SemanticAction::AcquireLease,
    SemanticAction::RenewLease,
    SemanticAction::ReleaseLease,
    SemanticAction::StartWorker,
    SemanticAction::HeartbeatWorker,
    SemanticAction::StopWorker,
    SemanticAction::CreateOperation,
    SemanticAction::ReportOperation,
    SemanticAction::CancelOperation,
    SemanticAction::PublishEndpoint,
    SemanticAction::AuthorizeEndpoint,
    SemanticAction::RevokeEndpoint,
    SemanticAction::SubscribeEvents,
];
