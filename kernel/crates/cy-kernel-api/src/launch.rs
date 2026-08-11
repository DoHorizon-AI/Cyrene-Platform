//! 进程沙箱启动计划与安装包解析端口.

use std::{collections::BTreeMap, path::PathBuf};

use cy_kernel_contract as semantic;

use crate::{error::ProviderError, lease::CgroupLimits};

/// 进程沙箱启动计划 (Launch Plan)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// 进程实例名称
    pub instance_name: String,
    /// 可执行程序文件路径
    pub executable: PathBuf,
    /// 启动命令行参数
    pub args: Vec<String>,
    /// 环境变量键值对
    pub environment: BTreeMap<String, String>,
    /// 分配给该进程的 cgroup 作用域组名
    pub cgroup_name: String,
    /// 进程启动前必须生效的 cgroup 配额。
    pub limits: CgroupLimits,
}

/// Immutable identity of an installation that has already passed installer
/// verification. It deliberately contains no product/plugin implementation
/// details, executable path, arguments, or environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedInstallation {
    pub installation_name: String,
    pub manifest_digest: String,
    pub artifact_digest: String,
    pub verified_signature_identity: String,
}

/// A launch plan accompanied by the exact immutable installation identity the
/// outer resolver verified. Kernel code compares this binding with the
/// `InstalledPluginRef` before it can hand the plan to sandboxd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunchPlan {
    pub installation: VerifiedInstallation,
    pub plan: LaunchPlan,
}

/// Port for obtaining a launch plan from a previously verified installation.
///
/// Install layout and record parsing belong to an outer adapter. Before the
/// result is launched, the Kernel still overlays lease limits, reserved
/// heartbeat values, and device-binding environment restrictions.
pub trait InstalledPluginResolver: Send + Sync {
    fn resolve_launch_plan(
        &self,
        installation: &VerifiedInstallation,
        instance_name: &str,
    ) -> Result<ResolvedLaunchPlan, ProviderError>;

    /// Resolve an opaque, externally verified Worker execution reference. The
    /// Kernel passes it through without parsing installation, OCI, language or
    /// runtime details; the outer resolver proves its immutable binding before
    /// returning a launch plan.
    fn resolve_worker_launch_plan(
        &self,
        worker: &semantic::Worker,
    ) -> Result<ResolvedLaunchPlan, ProviderError> {
        Err(ProviderError::new(
            "execution-plan-resolver",
            "EXECUTION_REFERENCE_UNRESOLVED",
            &worker.execution_ref,
        ))
    }
}
