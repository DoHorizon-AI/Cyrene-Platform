//! CYRENE 硬件拓扑与加速卡探测适配器 (Hardware Discovery Adapters).
//!
//! 【进程外无侵入式硬件探测设计】
//! 本模块实现了对宿主机 GPU / 加速芯片的探测。为了避免在 Rust 内核守护进程中直接链接厂商专有的动态链接库
//! （如 libnvidia-ml.so / NVML C SDK），从而导致内核与特定驱动版本强耦合甚至驱动崩溃带崩内核，
//! CYRENE 采用「进程外隔离探测（Process-Out-of-Kernel）」策略：
//! 1. [`NvidiaSmiProvider`]: 通过安全调用系统 `nvidia-smi` 命令行工具解析 CSV 输出获取显卡 UUID、显存、PCI 地址；
//! 2. 结合 Linux `/sys/bus/pci/devices/` sysfs 树解析 NUMA 亲和性节点；
//! 3. 扫描 `/dev/nvidia*` 字符设备节点并构建安全绑定的 [`DeviceBinding`]；
//! 4. [`parse_nvidia_topology`]: 解析 `nvidia-smi topo -m` 互联拓扑矩阵，识别 NVLink 与 PCIe P2P 链路。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use cy_kernel_api::{
    semantic::{Capability, Identity, Quantity, Resource, ResourceState},
    CapabilityFact, DeviceBinding, DeviceNode, HealthReport, HostInventoryProvider,
    InventorySnapshot, NodeCapabilities, ProviderError, ResourceProvider,
};

/// 命令行执行输出结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// 进程退出码
    pub status: i32,
    /// 标准输出文本
    pub stdout: String,
    /// 标准错误文本
    pub stderr: String,
}

/// 命令行运行抽象接口（用于依赖注入与 Mock 单测）
pub trait CommandRunner: Send + Sync {
    /// 执行指定程序并捕获输出
    fn run(&self, executable: &Path, args: &[String]) -> Result<CommandOutput, ProviderError>;
}

/// 基于操作系统标准 `std::process::Command` 的真实命令执行器
#[derive(Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, executable: &Path, args: &[String]) -> Result<CommandOutput, ProviderError> {
        let output = Command::new(executable)
            .args(args)
            .output()
            .map_err(|error| {
                ProviderError::new("nvidia-smi", "PROBE_EXEC_FAILED", &error.to_string())
            })?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// 基于 `nvidia-smi` 的 NVIDIA GPU 硬件探测适配器
pub struct NvidiaSmiProvider {
    /// 命令行执行器实例
    runner: Arc<dyn CommandRunner>,
    /// `nvidia-smi` 可执行程序路径
    command: PathBuf,
    /// 设备文件根目录（默认为 `/dev`）
    device_root: PathBuf,
    /// Sysfs 虚拟文件系统根目录（默认为 `/sys`）
    sysfs_root: PathBuf,
    /// Adapter 进程内维护的单调事实代次。相同快照不改变代次。
    inventory_generation: Mutex<InventoryGeneration>,
}

#[derive(Debug, Default)]
struct InventoryGeneration {
    generation: u64,
    fingerprint: String,
}

impl NvidiaSmiProvider {
    /// 创建 NVIDIA 硬件探测适配器实例
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Self {
            runner: Arc::new(SystemCommandRunner),
            command: command.into(),
            device_root: PathBuf::from("/dev"),
            sysfs_root: PathBuf::from("/sys"),
            inventory_generation: Mutex::new(InventoryGeneration::default()),
        }
    }

    /// 注入自定义命令执行器（主要用于测试）
    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Self {
        self.runner = runner;
        self
    }

    /// 自定义设备节点根路径
    pub fn with_device_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.device_root = root.into();
        self
    }

    /// 自定义 Sysfs 根路径
    pub fn with_sysfs_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sysfs_root = root.into();
        self
    }

    /// 调用 `nvidia-smi --query-gpu=...` 并解析基础硬件参数
    fn query(&self) -> Result<Vec<ParsedGpu>, ProviderError> {
        let args = vec![
            "--query-gpu=index,uuid,name,pci.bus_id,memory.total,memory.free".to_string(),
            "--format=csv,noheader,nounits".to_string(),
        ];
        let output = self.runner.run(&self.command, &args)?;
        if output.status != 0 {
            return Err(ProviderError::new(
                "nvidia-smi",
                "PROBE_FAILED",
                output.stderr.trim(),
            ));
        }
        parse_nvidia_query(&output.stdout)
    }

    /// 从 `/sys/bus/pci/devices/<pci>/numa_node` 读取 GPU 绑定的 NUMA 节点编号
    fn numa_node(&self, pci_address: &str) -> Option<i32> {
        let path = self
            .sysfs_root
            .join("bus/pci/devices")
            .join(pci_address)
            .join("numa_node");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok())
    }

    /// 扫描并定位指定 GPU 序号关联的操作系统设备文件（如 `/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm`）
    fn nodes_for(&self, index: usize) -> Vec<DeviceNode> {
        let mut nodes = vec![device_node(
            self.device_root.join(format!("nvidia{index}")),
            true,
        )];
        for name in ["nvidiactl", "nvidia-uvm", "nvidia-uvm-tools"] {
            let path = self.device_root.join(name);
            if path.exists() {
                nodes.push(device_node(path, true));
            }
        }
        nodes
    }

    fn next_inventory_generation(&self, resources: &[Resource]) -> u64 {
        let fingerprint = resources
            .iter()
            .map(|device| format!("{device:?}"))
            .collect::<String>();
        let mut state = self
            .inventory_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.generation == 0 || state.fingerprint != fingerprint {
            state.generation = state.generation.saturating_add(1).max(1);
            state.fingerprint = fingerprint;
        }
        state.generation
    }
}

impl ResourceProvider for NvidiaSmiProvider {
    fn adapter_id(&self) -> &str {
        "nvidia-smi"
    }

    fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
        self.query().map(|gpus| {
            gpus.into_iter()
                .map(|gpu| {
                    let mut capacity = BTreeMap::new();
                    if let Some(value) = gpu.total_memory_bytes {
                        capacity.insert(
                            "memory.total".to_string(),
                            Quantity {
                                value,
                                unit: "byte".to_string(),
                            },
                        );
                    }
                    if let Some(value) = gpu.free_memory_bytes {
                        capacity.insert(
                            "memory.allocatable".to_string(),
                            Quantity {
                                value,
                                unit: "byte".to_string(),
                            },
                        );
                    }
                    let mut attributes = BTreeMap::from([
                        ("vendor".to_string(), "nvidia".to_string()),
                        ("family".to_string(), gpu.name),
                        ("pci.address".to_string(), gpu.pci_address.clone()),
                    ]);
                    if let Some(numa_node) = self.numa_node(&gpu.pci_address) {
                        attributes.insert("numa.node".to_string(), numa_node.to_string());
                    }
                    Resource {
                        identity: Identity {
                            id: gpu.uuid,
                            generation: 1,
                        },
                        provider: Identity {
                            id: self.adapter_id().to_string(),
                            generation: 1,
                        },
                        resource_class: "accelerator".to_string(),
                        capabilities: vec![
                            Capability {
                                id: "accelerator.compute".to_string(),
                                revision: 1,
                                properties: BTreeMap::new(),
                            },
                            Capability {
                                id: "accelerator.kind.gpu".to_string(),
                                revision: 1,
                                properties: BTreeMap::new(),
                            },
                            Capability {
                                id: "vendor.nvidia.cuda".to_string(),
                                revision: 1,
                                properties: BTreeMap::new(),
                            },
                        ],
                        capacity,
                        attributes,
                        state: ResourceState::Ready,
                        reason_code: "nvidia-smi-probe-ok".to_string(),
                        summary: "provider returned a complete resource row".to_string(),
                        links: Vec::new(),
                    }
                })
                .collect()
        })
    }

    fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
        if resource.provider.id != self.adapter_id()
            || !resource
                .capabilities
                .iter()
                .any(|capability| capability.id == "vendor.nvidia.cuda")
        {
            return Err(ProviderError::new(
                self.adapter_id(),
                "RESOURCE_PROVIDER_MISMATCH",
                "resource was not published by this provider",
            ));
        }
        let gpu = self
            .query()?
            .into_iter()
            .find(|gpu| gpu.uuid == resource.identity.id)
            .ok_or_else(|| {
                ProviderError::new(
                    self.adapter_id(),
                    "RESOURCE_NOT_FOUND",
                    "resource is absent",
                )
            })?;
        let nodes = self.nodes_for(gpu.index);
        let missing = nodes
            .iter()
            .filter(|node| node.required && !node.path.exists())
            .map(|node| node.path.display().to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ProviderError::new(
                self.adapter_id(),
                "DEVICE_NODE_MISSING",
                &missing.join(", "),
            ));
        }
        let unresolved = nodes
            .iter()
            .filter(|node| node.required && (node.major.is_none() || node.minor.is_none()))
            .map(|node| node.path.display().to_string())
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            return Err(ProviderError::new(
                self.adapter_id(),
                "DEVICE_NODE_IDENTITY_UNKNOWN",
                &unresolved.join(", "),
            ));
        }

        let mut environment = BTreeMap::new();
        environment.insert(
            "CUDA_VISIBLE_DEVICES".to_string(),
            resource.identity.id.clone(),
        );
        environment.insert(
            "NVIDIA_VISIBLE_DEVICES".to_string(),
            resource.identity.id.clone(),
        );
        Ok(DeviceBinding {
            resource_id: resource.identity.id.clone(),
            nodes,
            environment,
            required_gids: Vec::new(),
            enforcement: cy_kernel_api::EnforcementMode::Hard,
            adapter_id: self.adapter_id().to_string(),
            reason_code: "DEVICE_BPF_REQUIRED".to_string(),
        })
    }

    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
        ResourceProvider::probe_resources(self)?
            .into_iter()
            .find(|resource| resource.identity.id == resource_id)
            .map(|resource| HealthReport {
                healthy: Some(resource.state == ResourceState::Ready),
                reason_code: resource.reason_code,
                summary: resource.summary,
            })
            .ok_or_else(|| {
                ProviderError::new(self.adapter_id(), "DEVICE_NOT_FOUND", "device is absent")
            })
    }
}

/// Capture the actual major/minor identity while still inside the isolated
/// hardware adapter. The Kernel re-checks it immediately before attaching its
/// cgroup-device eBPF filter, preventing a path swap from widening access.
fn device_node(path: PathBuf, required: bool) -> DeviceNode {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let device = std::fs::metadata(&path)
            .ok()
            .map(|metadata| metadata.rdev());
        let major = device.map(|value| ((value >> 8) & 0x0fff) as u32);
        let minor = device.map(|value| ((value & 0xff) | ((value >> 12) & 0x0fff00)) as u32);
        DeviceNode {
            path,
            major,
            minor,
            required,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        DeviceNode {
            path,
            major: None,
            minor: None,
            required,
        }
    }
}

impl HostInventoryProvider for NvidiaSmiProvider {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        let mut resources = <Self as ResourceProvider>::probe_resources(self)?;
        let generation = self.next_inventory_generation(&resources);
        for resource in &mut resources {
            resource.identity.generation = generation;
        }
        Ok(InventorySnapshot {
            generation,
            resources,
            capabilities: NodeCapabilities {
                ready: true,
                facts: vec![CapabilityFact {
                    name: "nvidia-smi".to_string(),
                    available: true,
                    required: false,
                    detail: "NVIDIA inventory is supplied by the isolated CLI adapter".to_string(),
                }],
                enforcement: Vec::new(),
            },
        })
    }
}

/// 内部结构体：解析自 `nvidia-smi` CSV 行的 GPU 原始信息
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedGpu {
    index: usize,
    uuid: String,
    name: String,
    pci_address: String,
    total_memory_bytes: Option<u64>,
    free_memory_bytes: Option<u64>,
}

/// 解析 `nvidia-smi --query-gpu=... --format=csv,noheader,nounits` 的文本行输出
fn parse_nvidia_query(output: &str) -> Result<Vec<ParsedGpu>, ProviderError> {
    let mut devices = Vec::new();
    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 6 || fields.iter().any(|field| field.is_empty()) {
            return Err(ProviderError::new(
                "nvidia-smi",
                "PROBE_INCOMPLETE",
                &format!(
                    "line {} does not contain six complete fields",
                    line_number + 1
                ),
            ));
        }
        let index = fields[0].parse::<usize>().map_err(|_| {
            ProviderError::new("nvidia-smi", "PROBE_INVALID", "GPU index is not numeric")
        })?;
        let total_memory_bytes = parse_mib(fields[4], "total memory")?;
        let free_memory_bytes = parse_mib(fields[5], "free memory")?;
        devices.push(ParsedGpu {
            index,
            uuid: fields[1].to_string(),
            name: fields[2].to_string(),
            pci_address: fields[3].to_string(),
            total_memory_bytes,
            free_memory_bytes,
        });
    }
    Ok(devices)
}

/// 解析 MiB 整数文本并换算为字节数 (Bytes)
fn parse_mib(value: &str, field: &str) -> Result<Option<u64>, ProviderError> {
    let value = value.trim();
    let mib = value.parse::<u64>().map_err(|_| {
        ProviderError::new(
            "nvidia-smi",
            "PROBE_INVALID",
            &format!("{field} is not an integer MiB value"),
        )
    })?;
    Ok(Some(mib.saturating_mul(1024 * 1024)))
}

/// 解析 `nvidia-smi topo -m` 互联拓扑矩阵文本，提取 GPU 间的 NVLink / PCIe P2P 链路关系
pub fn parse_nvidia_topology(output: &str) -> Vec<(String, String, String)> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let columns = header.split_whitespace().collect::<Vec<_>>();
    let mut links = Vec::new();
    for row in lines {
        let fields = row.split_whitespace().collect::<Vec<_>>();
        if fields.len() <= 1 {
            continue;
        }
        let source = fields[0].to_string();
        for (index, value) in fields.iter().skip(1).enumerate() {
            let Some(target) = columns.get(index) else {
                continue;
            };
            let link_type = match value.to_ascii_uppercase().as_str() {
                value if value.starts_with("NV") => Some("vendor.nvidia.nvlink".to_string()),
                value if value.starts_with("PIX") || value.starts_with("PHB") => {
                    Some("interconnect.pcie".to_string())
                }
                _ => None,
            };
            if let Some(link_type) = link_type {
                links.push((source.clone(), (*target).to_string(), link_type));
            }
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRunner {
        output: CommandOutput,
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            _executable: &Path,
            _args: &[String],
        ) -> Result<CommandOutput, ProviderError> {
            Ok(self.output.clone())
        }
    }

    struct SequenceRunner {
        outputs: Mutex<Vec<CommandOutput>>,
    }

    impl CommandRunner for SequenceRunner {
        fn run(
            &self,
            _executable: &Path,
            _args: &[String],
        ) -> Result<CommandOutput, ProviderError> {
            Ok(self.outputs.lock().unwrap().remove(0))
        }
    }

    #[test]
    fn nvidia_probe_keeps_stable_identity_and_does_not_fabricate_topology() {
        let provider = NvidiaSmiProvider::new("nvidia-smi").with_runner(Arc::new(FakeRunner {
            output: CommandOutput {
                status: 0,
                stdout: "0, GPU-uuid, NVIDIA A100, 00000000:01:00.0, 40960, 40000\n".into(),
                stderr: String::new(),
            },
        }));
        let resources = ResourceProvider::probe_resources(&provider).unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].identity.id, "GPU-uuid");
        assert_eq!(
            resources[0].capacity["memory.total"].value,
            40960 * 1024 * 1024
        );
        assert!(resources[0].links.is_empty());
        assert!(!resources[0].attributes.contains_key("numa.node"));
    }

    #[test]
    fn incomplete_nvidia_row_is_rejected() {
        let result = parse_nvidia_query("0, GPU-uuid, NVIDIA A100, , 40960, 40000");
        assert_eq!(result.unwrap_err().reason_code, "PROBE_INCOMPLETE");
    }

    #[test]
    fn topology_parser_returns_only_known_links() {
        let links = parse_nvidia_topology(
            "GPU0 GPU1 CPU Affinity\nGPU0 X NV1 SYS 0-7\nGPU1 NV1 X SYS 8-15",
        );
        assert_eq!(
            links,
            vec![
                ("GPU0".into(), "GPU1".into(), "vendor.nvidia.nvlink".into()),
                ("GPU1".into(), "GPU0".into(), "vendor.nvidia.nvlink".into()),
            ]
        );
    }

    #[test]
    fn inventory_generation_changes_only_when_observed_facts_change() {
        let normal = CommandOutput {
            status: 0,
            stdout: "0, GPU-uuid, NVIDIA A100, 00000000:01:00.0, 40960, 40000\n".into(),
            stderr: String::new(),
        };
        let changed = CommandOutput {
            status: 0,
            stdout: "0, GPU-uuid, NVIDIA A100, 00000000:01:00.0, 40960, 39000\n".into(),
            stderr: String::new(),
        };
        let provider = NvidiaSmiProvider::new("nvidia-smi").with_runner(Arc::new(SequenceRunner {
            outputs: Mutex::new(vec![normal.clone(), normal, changed]),
        }));
        let first = HostInventoryProvider::probe_inventory(&provider).unwrap();
        let second = HostInventoryProvider::probe_inventory(&provider).unwrap();
        let third = HostInventoryProvider::probe_inventory(&provider).unwrap();
        assert_eq!(first.generation, second.generation);
        assert!(third.generation > second.generation);
    }
}
