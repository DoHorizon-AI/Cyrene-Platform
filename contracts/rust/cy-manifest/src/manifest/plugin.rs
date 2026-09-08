// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/plugin.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 插件契约数据结构定义。
//!
//! 包含插件版本级别、运行宿主形式、重启策略、启动配置、安全权限、资源限制、
//! 元数据、依赖声明、能力清单、子组件及完整插件清单（PluginManifest）。
//!
//! `MIGRATING_COMPATIBILITY`: the controlled v0 plugin kinds and typed
//! capability tables remain frozen for implemented consumers. New plugins use
//! free capability identifiers, interface versions, and execution modes in
//! `CapabilityDescriptor`; do not add another named kind here.

use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};

/// 插件发布版本级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    /// 社区开源版本
    Community,
    /// 专业/企业商业版本
    Pro,
}

impl Edition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Edition::Community => "community",
            Edition::Pro => "pro",
        }
    }
}

impl std::fmt::Display for Edition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 插件运行宿主环境形式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// 作为独立子进程运行的 Python 插件 (stdio/IPC 通信)
    SubprocessPython,
    /// 作为独立子进程运行的 JVM 插件 (stdio/IPC 通信)
    SubprocessJvm,
    /// 长期驻留独立系统服务
    Service,
}

impl Runtime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Runtime::SubprocessPython => "subprocess-python",
            Runtime::SubprocessJvm => "subprocess-jvm",
            Runtime::Service => "service",
        }
    }

    pub fn is_subprocess(&self) -> bool {
        matches!(self, Runtime::SubprocessPython | Runtime::SubprocessJvm)
    }
}

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 插件异常退出时的重启策略
#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    /// 绝不重启
    Never,
    /// 异常失败时重启（带指数退避）
    OnFailure {
        /// 最大允许连续重启次数
        max_restarts: u32,
        /// 初始退避等待时长（毫秒）
        min_backoff_ms: u64,
        /// 最大退避等待时长（毫秒）
        max_backoff_ms: u64,
        /// 指数退避倍率因子
        factor: f64,
    },
    /// 总是重启
    Always,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy::OnFailure {
            max_restarts: 3,
            min_backoff_ms: 500,
            max_backoff_ms: 30000,
            factor: 2.0,
        }
    }
}

impl Serialize for RestartPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Never => "never",
            Self::OnFailure { .. } => "on-failure",
            Self::Always => "always",
        })
    }
}

impl<'de> Deserialize<'de> for RestartPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RestartPolicyVisitor;

        impl<'de> Visitor<'de> for RestartPolicyVisitor {
            type Value = RestartPolicy;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("never, on-failure, or always")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "never" => Ok(RestartPolicy::Never),
                    "on-failure" => Ok(RestartPolicy::default()),
                    "always" => Ok(RestartPolicy::Always),
                    _ => Err(E::custom(format!("unsupported restart policy: {value}"))),
                }
            }
        }

        deserializer.deserialize_str(RestartPolicyVisitor)
    }
}

/// 插件启动命令与进程参数配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginLaunch {
    /// 执行文件路径或二进制名称（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub executable: Option<String>,
    /// 命令行启动参数列表
    #[serde(default)]
    pub args: Vec<String>,
    /// 通信传输机制（例如："stdio", "uds", "tcp"，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub transport: Option<String>,
    /// 启动握手超时时间（毫秒，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub startup_timeout_ms: Option<u64>,
    /// 优雅退出超时时间（毫秒，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shutdown_timeout_ms: Option<u64>,
}

/// 插件安全权限申请声明
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPermissions {
    /// 允许读取的文件系统路径模式
    #[serde(default)]
    pub filesystem_read: Vec<String>,
    /// 允许写入的文件系统路径模式
    #[serde(default)]
    pub filesystem_write: Vec<String>,
    /// 允许出站网络访问的域名/IP 列表
    #[serde(default)]
    pub network: Vec<String>,
    /// 是否需要 GPU 访问权限
    #[serde(default)]
    pub gpu: bool,
    /// 是否允许衍生子进程
    #[serde(default)]
    pub process_spawn: bool,
}

/// 插件硬件资源配额与通信限制
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginResources {
    /// 最大允许使用的内存上限（MB，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_mb: Option<u64>,
    /// 单个 IPC 消息包最大字节数（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_message_bytes: Option<u64>,
}

/// 插件包分发与完整性签名信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPackage {
    /// 插件压缩包 SHA-256 校验和（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
    /// 数字签名（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
    /// 支持的目标操作系统（例如：["linux", "windows"]）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub target_os: Vec<String>,
    /// 支持的目标架构（例如：["x86_64", "aarch64"]）
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub target_arch: Vec<String>,
}

/// 插件基础元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    /// 插件唯一标识符（例如："com.cyrene.nvidia.probe"）
    pub id: String,
    /// 插件名称
    #[serde(default)]
    pub name: String,
    /// 插件版本号（语义化版本，例如："1.0.0"）
    pub version: String,
    /// 兼容的平台 API 版本（例如："1.0"）
    pub api_version: String,
    /// 插件类型分类（保持为 String 避免包循环依赖）
    pub kind: String,
    /// 发布版本（Community 或 Pro）
    pub edition: Edition,
    /// 运行宿主方式（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime: Option<Runtime>,
    /// 是否需要商业许可证门禁验证
    #[serde(default)]
    pub license_gate: bool,
    /// 插件入口类/函数路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub entrypoint: Option<String>,
    /// 插件功能描述（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// 插件作者（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub author: Option<String>,
    /// 开源/商业协议（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub license: Option<String>,
    /// 源码目标路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_target: Option<String>,
    /// 状态（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    /// 通信协议版本号（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub protocol_version: Option<u32>,
    /// 作用域范围（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope: Option<String>,
    /// 进程故障重启策略
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

impl PluginMetadata {
    pub fn plugin_id(&self) -> &str {
        &self.id
    }
}

/// 插件依赖关系声明
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginDependencies {
    /// 必须前置安装的其他插件 ID 列表
    #[serde(default, alias = "other_plugins")]
    pub required_plugins: Vec<String>,
    /// 可选集成的插件 ID 列表
    #[serde(default)]
    pub optional_plugins: Vec<String>,
    /// 互斥冲突的插件 ID 列表
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// 依赖的系统能力特性
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// 依赖的系统级动态库/可执行文件
    #[serde(default)]
    pub system_dependencies: Vec<String>,
    /// 依赖的 Python 第三方包
    #[serde(default)]
    pub python_packages: Vec<String>,
}

/// 插件所宣称支持的能力特性清单
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginCapabilitiesManifest {
    /// 支持的硬件型号列表
    #[serde(default)]
    pub supported_hardware: Vec<String>,
    /// 支持的计算精度
    #[serde(default)]
    pub supported_precisions: Vec<String>,
    /// 支持的量化方法
    #[serde(default)]
    pub supported_quantizations: Vec<String>,
    /// 是否支持流式传输 (Streaming)
    #[serde(default)]
    pub supports_streaming: bool,
    /// 扩展特性标签列表
    #[serde(default)]
    pub features: Vec<String>,
}

/// 组合包插件内包含的子组件定义
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginComponent {
    /// 组件唯一标识 ID
    pub id: String,
    /// 组件类型（对应扩展点分类，例如 "probe", "model-analyzer"）
    pub kind: String,
    /// 发布版本（Community 或 Pro）
    pub edition: Edition,
    /// 组件版本号
    pub version: String,
    /// 运行宿主环境方式（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime: Option<Runtime>,
    /// 是否需要商业许可证门禁验证
    #[serde(default)]
    pub license_gate: bool,
    /// 声明的能力特性标识列表
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 组件状态（例如 "active", "deprecated"，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    /// 源码目标路径（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source_target: Option<String>,
}

/// 完整插件清单（PluginManifest）：描述插件元数据、能力、依赖、安全策略及进程启动参数
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// 基础元数据
    pub plugin: PluginMetadata,
    /// 能力声明
    #[serde(default)]
    pub capabilities: PluginCapabilitiesManifest,
    /// 依赖关系
    #[serde(default)]
    pub dependencies: PluginDependencies,
    /// 包含的组件列表
    #[serde(default)]
    pub components: Vec<PluginComponent>,

    /// 进程启动配置（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub launch: Option<PluginLaunch>,
    /// 权限声明（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub permissions: Option<PluginPermissions>,
    /// 资源限制（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resources: Option<PluginResources>,
    /// 打包分发属性（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub package: Option<PluginPackage>,

    /// Stable capability declarations used by the Platform resolver.
    ///
    /// The legacy `capabilities` table remains intact for backwards
    /// compatibility; these descriptors carry the matchable interface
    /// contract and execution modes.
    #[serde(default)]
    pub capability_descriptors: Vec<CapabilityDescriptor>,
    /// Content-addressed artifact reference when one is available.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<PluginArtifactRef>,
}

/// Stable identity of a plugin, independent of its release version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIdentity {
    pub id: String,
}

impl PluginIdentity {
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("plugin identity must not be empty".to_string());
        }
        Ok(Self { id })
    }
}

/// Published plugin version. Ordering is lexical in the MVP and therefore
/// deterministic; semantic-version policy remains a follow-up extension.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginVersion {
    pub version: String,
}

impl PluginVersion {
    pub fn new(version: impl Into<String>) -> Result<Self, String> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err("plugin version must not be empty".to_string());
        }
        Ok(Self { version })
    }
}

/// Stable capability name, for example `training.engine.v1`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityId {
    pub id: String,
}

impl CapabilityId {
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err("capability id must not be empty".to_string());
        }
        Ok(Self { id })
    }
}

/// Version of the callable capability interface, not the plugin release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInterfaceVersion {
    pub version: String,
}

impl CapabilityInterfaceVersion {
    pub fn new(version: impl Into<String>) -> Result<Self, String> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err("capability interface version must not be empty".to_string());
        }
        Ok(Self { version })
    }
}

/// Reference to a materialized plugin artifact. The resolver never downloads
/// or installs it; it only carries the reference into the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginArtifactRef {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<String>,
}

/// Supported execution placement for a resolved capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ExecutionMode {
    Inline,
    Worker,
    Service,
}

impl ExecutionMode {
    pub const ALL: [Self; 3] = [Self::Inline, Self::Worker, Self::Service];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "INLINE",
            Self::Worker => "WORKER",
            Self::Service => "SERVICE",
        }
    }
}

/// Matchable capability declaration owned by the plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    #[serde(default)]
    pub execution_modes: Vec<ExecutionMode>,
}

impl CapabilityDescriptor {
    pub fn new(
        id: CapabilityId,
        interface_version: CapabilityInterfaceVersion,
        execution_modes: impl IntoIterator<Item = ExecutionMode>,
    ) -> Result<Self, String> {
        let mut modes: Vec<_> = execution_modes.into_iter().collect();
        modes.sort();
        modes.dedup();
        if modes.is_empty() {
            return Err("capability descriptor must declare an execution mode".to_string());
        }
        Ok(Self {
            id,
            interface_version,
            execution_modes: modes,
        })
    }

    pub fn supports_mode(&self, mode: ExecutionMode) -> bool {
        self.execution_modes.contains(&mode)
    }
}

/// Capability requirement used by a PluginSet and by service adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRequirement {
    pub capability: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    /// Empty means any supported mode; otherwise the resolver must select one
    /// of these modes in the deterministic order INLINE, WORKER, SERVICE.
    #[serde(default)]
    pub execution_modes: Vec<ExecutionMode>,
}

/// Name used by service-facing code when the requirement is capability-first.
pub type CapabilityRequirement = PluginRequirement;

/// Logical set of capability requirements. It does not imply one runtime or
/// Python environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetSpec {
    #[serde(default)]
    pub requirements: Vec<PluginRequirement>,
    #[serde(default)]
    pub allowed_execution_modes: Vec<ExecutionMode>,
}

/// One deterministic resolution result and its compatibility evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCapability {
    pub plugin: PluginIdentity,
    pub plugin_version: PluginVersion,
    pub capability: CapabilityId,
    pub interface_version: CapabilityInterfaceVersion,
    pub execution_mode: ExecutionMode,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact: Option<PluginArtifactRef>,
    #[serde(default)]
    pub compatibility_evidence: Vec<String>,
}

/// Immutable reference lock for a resolved logical PluginSet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSetLock {
    pub contract_version: String,
    pub entries: Vec<ResolvedCapability>,
    /// Digest is derived from the lock without this field and is not part of
    /// its own preimage.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub digest: Option<String>,
}

impl PluginManifest {
    /// Return the v1 artifact reference, including a digest from the legacy
    /// package table when no explicit v1 artifact field is present.
    pub fn artifact_reference(&self) -> Option<PluginArtifactRef> {
        if let Some(artifact) = &self.artifact {
            return Some(artifact.clone());
        }
        self.package
            .as_ref()
            .and_then(|package| package.sha256.clone())
            .map(|digest| PluginArtifactRef {
                uri: format!("plugin://{}", self.plugin.id),
                digest: Some(digest),
            })
    }
}

impl PluginSetLock {
    pub fn new(mut entries: Vec<ResolvedCapability>) -> Self {
        entries.sort_by(|left, right| {
            (
                &left.capability.id,
                &left.interface_version.version,
                &left.plugin.id,
                &left.plugin_version.version,
                left.execution_mode,
            )
                .cmp(&(
                    &right.capability.id,
                    &right.interface_version.version,
                    &right.plugin.id,
                    &right.plugin_version.version,
                    right.execution_mode,
                ))
        });
        Self {
            contract_version: "cyrene.plugin.v1".to_string(),
            entries,
            digest: None,
        }
    }

    pub fn with_digest(mut self) -> Result<Self, String> {
        self.digest = None;
        let value = serde_json::to_value(&self).map_err(|error| error.to_string())?;
        self.digest = Some(format!("sha256:{}", crate::canonical_sha256_hex(&value)));
        Ok(self)
    }
}
