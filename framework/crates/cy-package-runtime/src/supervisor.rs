use std::{collections::BTreeSet, collections::HashMap, time::Duration};

use cy_manifest::PluginManifest;
use cy_platform_api::{
    CapabilityWorkerActivator, CapabilityWorkerClient, NeverCancelled, WorkerActivationOptions,
};

use crate::{BindingId, InstallationId, PackageRuntimeError, RuntimeGeneration, RuntimeState};

/// Execution authority consumed by the package lifecycle.
///
/// Implementations must report process facts; callers must not derive RUNNING
/// from installation or Product enablement state.
pub trait WorkerSupervisor: Send {
    fn activate(
        &mut self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        manifest: &PluginManifest,
        options: WorkerActivationOptions,
        generation: RuntimeGeneration,
    ) -> Result<(), PackageRuntimeError>;

    fn deactivate(&mut self, binding_id: &BindingId) -> Result<(), PackageRuntimeError>;

    fn status(&mut self, binding_id: &BindingId) -> Result<RuntimeState, PackageRuntimeError>;

    fn active_bindings(&mut self) -> Result<BTreeSet<BindingId>, PackageRuntimeError>;

    fn invoke(
        &mut self,
        binding_id: &BindingId,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, PackageRuntimeError>;
}

struct SupervisedWorker {
    installation_id: InstallationId,
    generation: RuntimeGeneration,
    client: CapabilityWorkerClient,
}

/// Node-local worker supervision backed by the existing canonical Platform
/// capability worker activator and protocol client.
pub struct PlatformWorkerSupervisor {
    workers: HashMap<BindingId, SupervisedWorker>,
    shutdown_grace_period: Duration,
}

impl PlatformWorkerSupervisor {
    pub fn new(shutdown_grace_period: Duration) -> Self {
        Self {
            workers: HashMap::new(),
            shutdown_grace_period,
        }
    }
}

impl Default for PlatformWorkerSupervisor {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}

impl WorkerSupervisor for PlatformWorkerSupervisor {
    fn activate(
        &mut self,
        binding_id: &BindingId,
        installation_id: &InstallationId,
        manifest: &PluginManifest,
        options: WorkerActivationOptions,
        generation: RuntimeGeneration,
    ) -> Result<(), PackageRuntimeError> {
        self.deactivate(binding_id)?;
        let client = CapabilityWorkerActivator::activate_from_manifest(manifest, &options)
            .map_err(|error| PackageRuntimeError::new(error.code(), error.message().to_string()))?;
        self.workers.insert(
            binding_id.clone(),
            SupervisedWorker {
                installation_id: installation_id.clone(),
                generation,
                client,
            },
        );
        Ok(())
    }

    fn deactivate(&mut self, binding_id: &BindingId) -> Result<(), PackageRuntimeError> {
        if let Some(mut worker) = self.workers.remove(binding_id) {
            worker
                .client
                .shutdown(self.shutdown_grace_period)
                .map_err(|error| {
                    PackageRuntimeError::new(error.code(), error.message().to_string())
                })?;
        }
        Ok(())
    }

    fn status(&mut self, binding_id: &BindingId) -> Result<RuntimeState, PackageRuntimeError> {
        let Some(worker) = self.workers.get_mut(binding_id) else {
            return Ok(RuntimeState::Stopped);
        };
        worker
            .client
            .is_running()
            .map_err(|error| PackageRuntimeError::new(error.code(), error.message().to_string()))
            .map(|running| {
                if running {
                    RuntimeState::Running
                } else {
                    RuntimeState::Failed
                }
            })
    }

    fn active_bindings(&mut self) -> Result<BTreeSet<BindingId>, PackageRuntimeError> {
        let mut active = BTreeSet::new();
        for (binding_id, worker) in &mut self.workers {
            if worker.client.is_running().map_err(|error| {
                PackageRuntimeError::new(error.code(), error.message().to_string())
            })? {
                active.insert(binding_id.clone());
            }
            let _ = (&worker.installation_id, worker.generation);
        }
        Ok(active)
    }

    fn invoke(
        &mut self,
        binding_id: &BindingId,
        capability: &str,
        method: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, PackageRuntimeError> {
        let worker = self.workers.get_mut(binding_id).ok_or_else(|| {
            PackageRuntimeError::new(
                "WORKER_UNAVAILABLE",
                format!("binding {binding_id} has no supervised worker"),
            )
        })?;
        worker
            .client
            .invoke(capability, method, payload, timeout, &NeverCancelled)
            .map_err(|error| PackageRuntimeError::new(error.code(), error.message().to_string()))
    }
}

impl Drop for PlatformWorkerSupervisor {
    fn drop(&mut self) {
        for (_, mut worker) in self.workers.drain() {
            let _ = worker.client.shutdown(self.shutdown_grace_period);
        }
    }
}
