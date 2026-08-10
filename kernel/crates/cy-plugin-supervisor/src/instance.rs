use std::sync::Arc;

use cy_kernel_api::{
    CleanupReport, DeviceBinding, LaunchPlan, ProcessHandle, ProviderError, SandboxBackend,
    StopRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedInstanceState {
    Discovered,
    Starting,
    Healthy,
    Stopping,
    Stopped,
    Quarantined,
}

pub struct ManagedInstance {
    runtime: Arc<dyn SandboxBackend>,
    plan: LaunchPlan,
    binding: DeviceBinding,
    handle: Option<ProcessHandle>,
    state: ManagedInstanceState,
    last_cleanup: Option<CleanupReport>,
}

impl ManagedInstance {
    pub fn new(runtime: Arc<dyn SandboxBackend>, plan: LaunchPlan, binding: DeviceBinding) -> Self {
        Self {
            runtime,
            plan,
            binding,
            handle: None,
            state: ManagedInstanceState::Discovered,
            last_cleanup: None,
        }
    }

    pub fn state(&self) -> ManagedInstanceState {
        self.state
    }

    pub fn last_cleanup(&self) -> Option<&CleanupReport> {
        self.last_cleanup.as_ref()
    }

    pub fn start(&mut self) -> Result<&ProcessHandle, ProviderError> {
        self.state = ManagedInstanceState::Starting;
        match self.runtime.launch(&self.plan, &self.binding) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.state = ManagedInstanceState::Healthy;
                Ok(self.handle.as_ref().expect("handle was just stored"))
            }
            Err(error) => {
                self.state = ManagedInstanceState::Quarantined;
                Err(error)
            }
        }
    }

    pub fn stop(&mut self, request: &StopRequest) -> Result<&CleanupReport, ProviderError> {
        let Some(handle) = self.handle.as_ref() else {
            self.state = ManagedInstanceState::Stopped;
            self.last_cleanup = Some(CleanupReport {
                complete: true,
                exit_code: None,
                oom_killed: false,
                conditions: Vec::new(),
                reason_code: "NOT_RUNNING".to_string(),
            });
            return Ok(self.last_cleanup.as_ref().expect("cleanup was just stored"));
        };
        self.state = ManagedInstanceState::Stopping;
        let report = self.runtime.stop(handle, request)?;
        self.state = if report.complete {
            ManagedInstanceState::Stopped
        } else {
            ManagedInstanceState::Quarantined
        };
        self.last_cleanup = Some(report);
        Ok(self.last_cleanup.as_ref().expect("cleanup was just stored"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cy_kernel_api::{
        CapabilityFact, EnforcementMode, EnforcementReport, NodeCapabilities, ProcessCondition,
        ProcessRuntime,
    };
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};

    struct FakeBackend {
        complete: bool,
    }

    impl ProcessRuntime for FakeBackend {
        fn preflight(&self) -> NodeCapabilities {
            NodeCapabilities {
                ready: true,
                facts: vec![CapabilityFact {
                    name: "test".into(),
                    available: true,
                    required: true,
                    detail: "test".into(),
                }],
                enforcement: vec![EnforcementReport {
                    resource_kind: "process-tree".into(),
                    mode: EnforcementMode::Hard,
                    adapter_id: "test".into(),
                    reason_code: "TEST".into(),
                }],
            }
        }

        fn launch(
            &self,
            _plan: &LaunchPlan,
            _binding: &DeviceBinding,
        ) -> Result<ProcessHandle, ProviderError> {
            Ok(ProcessHandle {
                pid: 42,
                cgroup_path: PathBuf::from("/test"),
                start_time_ticks: Some(1),
            })
        }

        fn stop(
            &self,
            _handle: &ProcessHandle,
            _request: &StopRequest,
        ) -> Result<CleanupReport, ProviderError> {
            Ok(CleanupReport {
                complete: self.complete,
                exit_code: None,
                oom_killed: false,
                conditions: if self.complete {
                    Vec::new()
                } else {
                    vec![ProcessCondition {
                        reason_code: "REAP_TIMEOUT".into(),
                        summary: "stuck".into(),
                    }]
                },
                reason_code: if self.complete {
                    "CLEANUP_COMPLETE".into()
                } else {
                    "PROCESS_UNINTERRUPTIBLE".into()
                },
            })
        }
    }

    impl SandboxBackend for FakeBackend {
        fn backend_id(&self) -> &str {
            "test"
        }
    }

    fn instance(complete: bool) -> ManagedInstance {
        ManagedInstance::new(
            Arc::new(FakeBackend { complete }),
            LaunchPlan {
                instance_name: "instance-1".into(),
                executable: PathBuf::from("worker"),
                args: Vec::new(),
                environment: BTreeMap::new(),
                cgroup_name: "instance-1".into(),
            },
            DeviceBinding {
                device_id: "gpu-0".into(),
                nodes: Vec::new(),
                environment: BTreeMap::new(),
                required_gids: Vec::new(),
                enforcement: EnforcementMode::VisibilityOnly,
                adapter_id: "test".into(),
                reason_code: "TEST".into(),
            },
        )
    }

    #[test]
    fn incomplete_cleanup_never_publishes_stopped() {
        let mut instance = instance(false);
        instance.start().unwrap();
        let report = instance
            .stop(&StopRequest {
                grace_period: Duration::from_millis(1),
                immediate: false,
            })
            .unwrap();
        assert!(!report.complete);
        assert_eq!(instance.state(), ManagedInstanceState::Quarantined);
    }

    #[test]
    fn complete_cleanup_publishes_stopped() {
        let mut instance = instance(true);
        instance.start().unwrap();
        let report = instance
            .stop(&StopRequest {
                grace_period: Duration::from_millis(1),
                immediate: false,
            })
            .unwrap();
        assert!(report.complete);
        assert_eq!(instance.state(), ManagedInstanceState::Stopped);
    }
}
