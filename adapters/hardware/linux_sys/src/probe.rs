// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/linux_sys/src/probe.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Linux host system facts, CPU, memory, NUMA, and OS capability probe.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Mutex,
};

use cy_kernel_contract::{
    semantic::{Capability, Identity, Quantity, Resource, ResourceState},
    CapabilityFact, DeviceBinding, EnforcementMode, EnforcementReport, HealthReport,
    HostInventoryProvider, InventorySnapshot, NodeCapabilities, ProviderError, ResourceProvider,
};

/// Parsed host CPU facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCpuInfo {
    pub model_name: String,
    pub architecture: String,
    pub logical_cores: u64,
    pub physical_cores: u64,
    pub flags: Vec<String>,
}

/// Parsed host memory facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

/// Host Linux system provider.
pub struct LinuxSystemProvider {
    adapter_id: String,
    procfs_root: PathBuf,
    sysfs_root: PathBuf,
    cgroup_root: PathBuf,
    inventory_generation: Mutex<InventoryGeneration>,
}

#[derive(Debug, Default)]
struct InventoryGeneration {
    generation: u64,
    fingerprint: String,
}

impl LinuxSystemProvider {
    pub fn new(adapter_id: impl Into<String>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            procfs_root: PathBuf::from("/proc"),
            sysfs_root: PathBuf::from("/sys"),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            inventory_generation: Mutex::new(InventoryGeneration::default()),
        }
    }

    pub fn with_procfs_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.procfs_root = root.into();
        self
    }

    pub fn with_sysfs_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sysfs_root = root.into();
        self
    }

    pub fn with_cgroup_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.cgroup_root = root.into();
        self
    }

    /// Read and parse `/proc/cpuinfo`.
    pub fn probe_cpu(&self) -> Result<ParsedCpuInfo, ProviderError> {
        let path = self.procfs_root.join("cpuinfo");
        let content = fs::read_to_string(&path).map_err(|err| {
            ProviderError::new(
                &self.adapter_id,
                "CPUINFO_READ_FAILED",
                &format!("failed to read {}: {}", path.display(), err),
            )
        })?;
        parse_cpuinfo(&content)
    }

    /// Read and parse `/proc/meminfo`.
    pub fn probe_memory(&self) -> Result<ParsedMemInfo, ProviderError> {
        let path = self.procfs_root.join("meminfo");
        let content = fs::read_to_string(&path).map_err(|err| {
            ProviderError::new(
                &self.adapter_id,
                "MEMINFO_READ_FAILED",
                &format!("failed to read {}: {}", path.display(), err),
            )
        })?;
        parse_meminfo(&content)
    }

    /// Read NUMA node count from `/sys/devices/system/node`.
    pub fn probe_numa_nodes(&self) -> usize {
        let path = self.sysfs_root.join("devices/system/node");
        if let Ok(entries) = fs::read_dir(path) {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|name| name.starts_with("node") && name[4..].parse::<u32>().is_ok())
                        .unwrap_or(false)
                })
                .count()
        } else {
            1
        }
    }

    /// Read Linux kernel release string via `uname`.
    pub fn probe_kernel_release(&self) -> String {
        #[cfg(target_os = "linux")]
        {
            let mut name = libc::utsname {
                sysname: [0; 65],
                nodename: [0; 65],
                release: [0; 65],
                version: [0; 65],
                machine: [0; 65],
                domainname: [0; 65],
            };
            if unsafe { libc::uname(&mut name) } == 0 {
                let release_cstr = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) };
                return release_cstr.to_string_lossy().into_owned();
            }
        }
        std::env::consts::OS.to_string()
    }

    /// Probe OS capability facts.
    pub fn probe_capabilities(&self) -> NodeCapabilities {
        let controllers_file = self.cgroup_root.join("cgroup.controllers");
        let controllers_content = fs::read_to_string(&controllers_file).ok();
        let cgroup_v2 = controllers_content.is_some();
        let has_controller = |name: &str| {
            controllers_content
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .any(|c| c == name)
        };
        let cgroup_kill = self.cgroup_root.join("cgroup.kill").is_file();

        let pidfd_supported = probe_pidfd_available();
        let numa_count = self.probe_numa_nodes();
        let kernel_release = self.probe_kernel_release();

        let facts = vec![
            CapabilityFact {
                name: "cgroup-v2".to_string(),
                available: cgroup_v2,
                required: true,
                detail: if cgroup_v2 {
                    format!(
                        "controllers: {}",
                        controllers_content.as_deref().unwrap_or_default().trim()
                    )
                } else {
                    "cgroup v2 controllers not mounted".to_string()
                },
            },
            CapabilityFact {
                name: "cgroup-kill".to_string(),
                available: cgroup_kill,
                required: false,
                detail: "cgroup.kill file present in cgroup v2 root".to_string(),
            },
            CapabilityFact {
                name: "cpu-controller".to_string(),
                available: has_controller("cpu"),
                required: false,
                detail: "cpu controller in cgroup.controllers".to_string(),
            },
            CapabilityFact {
                name: "memory-controller".to_string(),
                available: has_controller("memory"),
                required: false,
                detail: "memory controller in cgroup.controllers".to_string(),
            },
            CapabilityFact {
                name: "pids-controller".to_string(),
                available: has_controller("pids"),
                required: false,
                detail: "pids controller in cgroup.controllers".to_string(),
            },
            CapabilityFact {
                name: "pidfd".to_string(),
                available: pidfd_supported,
                required: false,
                detail: "Linux pidfd_open syscall supported".to_string(),
            },
            CapabilityFact {
                name: "os-kernel-release".to_string(),
                available: true,
                required: false,
                detail: kernel_release,
            },
            CapabilityFact {
                name: "numa-nodes".to_string(),
                available: numa_count > 0,
                required: false,
                detail: format!("NUMA nodes detected: {numa_count}"),
            },
        ];

        let ready = cgroup_v2;
        NodeCapabilities {
            ready,
            facts,
            enforcement: vec![EnforcementReport {
                resource_kind: "system-host".to_string(),
                mode: if ready {
                    EnforcementMode::Soft
                } else {
                    EnforcementMode::Unenforced
                },
                adapter_id: self.adapter_id.clone(),
                reason_code: if ready {
                    "LINUX_SYS_PROBE_READY".to_string()
                } else {
                    "CGROUP_V2_UNAVAILABLE".to_string()
                },
            }],
        }
    }

    fn next_inventory_generation(&self, resources: &[Resource]) -> u64 {
        let fingerprint = resources
            .iter()
            .map(|res| format!("{res:?}"))
            .collect::<String>();
        let mut state = self
            .inventory_generation
            .lock()
            .expect("inventory generation lock poisoned");
        if state.generation == 0 || state.fingerprint != fingerprint {
            state.generation = state.generation.saturating_add(1);
            state.fingerprint = fingerprint;
        }
        state.generation
    }
}

impl HostInventoryProvider for LinuxSystemProvider {
    fn probe_inventory(&self) -> Result<InventorySnapshot, ProviderError> {
        let resources = self.probe_resources()?;
        let generation = self.next_inventory_generation(&resources);
        let capabilities = self.probe_capabilities();
        Ok(InventorySnapshot {
            generation,
            resources,
            capabilities,
        })
    }
}

impl ResourceProvider for LinuxSystemProvider {
    fn adapter_id(&self) -> &str {
        &self.adapter_id
    }

    fn probe_resources(&self) -> Result<Vec<Resource>, ProviderError> {
        let cpu_info = self.probe_cpu()?;
        let mem_info = self.probe_memory()?;
        let numa_nodes = self.probe_numa_nodes();
        let kernel_release = self.probe_kernel_release();

        let mut cpu_attributes = BTreeMap::new();
        cpu_attributes.insert("cpu.model".to_string(), cpu_info.model_name.clone());
        cpu_attributes.insert("cpu.arch".to_string(), cpu_info.architecture.clone());
        cpu_attributes.insert("kernel.release".to_string(), kernel_release.clone());
        cpu_attributes.insert("numa.nodes".to_string(), numa_nodes.to_string());

        let mut cpu_capacity = BTreeMap::new();
        cpu_capacity.insert(
            "cores".to_string(),
            Quantity {
                value: cpu_info.logical_cores,
                unit: "cores".to_string(),
            },
        );
        cpu_capacity.insert(
            "physical_cores".to_string(),
            Quantity {
                value: cpu_info.physical_cores,
                unit: "cores".to_string(),
            },
        );

        let clean_arch = cpu_info.architecture.to_ascii_lowercase().replace('_', "");
        let cpu_capabilities = vec![
            Capability {
                id: "compute.cpu".to_string(),
                revision: 1,
                properties: BTreeMap::new(),
            },
            Capability {
                id: format!("cpu.arch.{clean_arch}"),
                revision: 1,
                properties: BTreeMap::new(),
            },
        ];

        let cpu_resource = Resource {
            identity: Identity {
                id: "cpu-host".to_string(),
                generation: 1,
            },
            provider: Identity {
                id: self.adapter_id.clone(),
                generation: 1,
            },
            resource_class: "compute.cpu".to_string(),
            capabilities: cpu_capabilities,
            capacity: cpu_capacity,
            attributes: cpu_attributes,
            state: ResourceState::Ready,
            reason_code: "cpu-probe-ok".to_string(),
            summary: format!(
                "{} ({} logical / {} physical cores)",
                cpu_info.model_name, cpu_info.logical_cores, cpu_info.physical_cores
            ),
            links: Vec::new(),
        };

        let mut mem_attributes = BTreeMap::new();
        mem_attributes.insert(
            "swap.enabled".to_string(),
            (mem_info.swap_total_bytes > 0).to_string(),
        );
        mem_attributes.insert("numa.nodes".to_string(), numa_nodes.to_string());

        let mut mem_capacity = BTreeMap::new();
        mem_capacity.insert(
            "capacity_bytes".to_string(),
            Quantity {
                value: mem_info.total_bytes,
                unit: "bytes".to_string(),
            },
        );
        mem_capacity.insert(
            "available_bytes".to_string(),
            Quantity {
                value: mem_info.available_bytes,
                unit: "bytes".to_string(),
            },
        );
        if mem_info.swap_total_bytes > 0 {
            mem_capacity.insert(
                "swap_total_bytes".to_string(),
                Quantity {
                    value: mem_info.swap_total_bytes,
                    unit: "bytes".to_string(),
                },
            );
            mem_capacity.insert(
                "swap_free_bytes".to_string(),
                Quantity {
                    value: mem_info.swap_free_bytes,
                    unit: "bytes".to_string(),
                },
            );
        }

        let mem_resource = Resource {
            identity: Identity {
                id: "ram-host".to_string(),
                generation: 1,
            },
            provider: Identity {
                id: self.adapter_id.clone(),
                generation: 1,
            },
            resource_class: "memory.ram".to_string(),
            capabilities: vec![Capability {
                id: "memory.system".to_string(),
                revision: 1,
                properties: BTreeMap::new(),
            }],
            capacity: mem_capacity,
            attributes: mem_attributes,
            state: if mem_info.available_bytes < 64 * 1024 * 1024 {
                ResourceState::Degraded
            } else {
                ResourceState::Ready
            },
            reason_code: "mem-probe-ok".to_string(),
            summary: format!(
                "{:.2} GiB RAM total, {:.2} GiB available",
                mem_info.total_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                mem_info.available_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
            links: Vec::new(),
        };

        Ok(vec![cpu_resource, mem_resource])
    }

    fn create_binding(&self, resource: &Resource) -> Result<DeviceBinding, ProviderError> {
        if resource.provider.id != self.adapter_id {
            return Err(ProviderError::new(
                &self.adapter_id,
                "RESOURCE_PROVIDER_MISMATCH",
                "resource was not published by this provider",
            ));
        }

        // Host CPU / RAM are managed by kernel cgroup allocations, requiring no /dev device node bindings.
        Ok(DeviceBinding {
            resource_id: resource.identity.id.clone(),
            nodes: Vec::new(),
            environment: BTreeMap::new(),
            joinable_environment_keys: std::collections::BTreeSet::new(),
            required_gids: Vec::new(),
            enforcement: EnforcementMode::Soft,
            adapter_id: self.adapter_id.clone(),
            reason_code: "LINUX_SYSTEM_RESOURCE_BINDING_ESTABLISHED".to_string(),
        })
    }

    fn read_health(&self, resource_id: &str) -> Result<HealthReport, ProviderError> {
        match resource_id {
            "cpu-host" => Ok(HealthReport {
                healthy: Some(true),
                reason_code: "HEALTHY".to_string(),
                summary: "host CPU is online".to_string(),
            }),
            "ram-host" => {
                let mem = self.probe_memory()?;
                if mem.available_bytes < 32 * 1024 * 1024 {
                    Ok(HealthReport {
                        healthy: Some(false),
                        reason_code: "MEMORY_CRITICALLY_LOW".to_string(),
                        summary: "available memory is below 32MiB".to_string(),
                    })
                } else {
                    Ok(HealthReport {
                        healthy: Some(true),
                        reason_code: "HEALTHY".to_string(),
                        summary: "memory availability is within nominal range".to_string(),
                    })
                }
            }
            _ => Err(ProviderError::new(
                &self.adapter_id,
                "RESOURCE_NOT_FOUND",
                &format!("unknown resource: {resource_id}"),
            )),
        }
    }
}

/// Parse `/proc/cpuinfo` text into `ParsedCpuInfo`.
pub fn parse_cpuinfo(content: &str) -> Result<ParsedCpuInfo, ProviderError> {
    let mut model_name = String::new();
    let mut logical_cores = 0;
    let mut physical_ids = BTreeSet::new();
    let mut core_ids = BTreeSet::new();
    let mut flags = Vec::new();

    let mut current_physical_id: Option<String> = None;
    let mut current_core_id: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            if let (Some(phys), Some(core)) = (current_physical_id.take(), current_core_id.take()) {
                core_ids.insert(format!("{phys}:{core}"));
            }
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "processor" => {
                    logical_cores += 1;
                }
                "model name" | "Model Name" | "Hardware" => {
                    if model_name.is_empty() {
                        model_name = value.to_string();
                    }
                }
                "physical id" => {
                    physical_ids.insert(value.to_string());
                    current_physical_id = Some(value.to_string());
                }
                "core id" => {
                    current_core_id = Some(value.to_string());
                }
                "flags" | "Features" if flags.is_empty() => {
                    flags = value.split_whitespace().map(|s| s.to_string()).collect();
                }
                _ => {}
            }
        }
    }

    if let (Some(phys), Some(core)) = (current_physical_id.take(), current_core_id.take()) {
        core_ids.insert(format!("{phys}:{core}"));
    }

    if logical_cores == 0 {
        logical_cores = 1;
    }
    let physical_cores = if !core_ids.is_empty() {
        core_ids.len() as u64
    } else if !physical_ids.is_empty() {
        physical_ids.len() as u64
    } else {
        logical_cores
    };

    if model_name.is_empty() {
        model_name = std::env::consts::ARCH.to_string();
    }

    Ok(ParsedCpuInfo {
        model_name,
        architecture: std::env::consts::ARCH.to_string(),
        logical_cores,
        physical_cores,
        flags,
    })
}

/// Parse `/proc/meminfo` text into `ParsedMemInfo`.
pub fn parse_meminfo(content: &str) -> Result<ParsedMemInfo, ProviderError> {
    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;
    let mut free_kb: Option<u64> = None;
    let mut buffers_kb: Option<u64> = None;
    let mut cached_kb: Option<u64> = None;
    let mut swap_total_kb: Option<u64> = None;
    let mut swap_free_kb: Option<u64> = None;

    for line in content.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value_part = value.split_whitespace().next().unwrap_or("0");
            let kb: u64 = value_part.parse().unwrap_or(0);
            match key {
                "MemTotal" => total_kb = Some(kb),
                "MemAvailable" => available_kb = Some(kb),
                "MemFree" => free_kb = Some(kb),
                "Buffers" => buffers_kb = Some(kb),
                "Cached" => cached_kb = Some(kb),
                "SwapTotal" => swap_total_kb = Some(kb),
                "SwapFree" => swap_free_kb = Some(kb),
                _ => {}
            }
        }
    }

    let total = total_kb.ok_or_else(|| {
        ProviderError::new("meminfo", "PARSE_ERROR", "MemTotal not found in meminfo")
    })? * 1024;

    let available = available_kb
        .or_else(|| {
            // Fallback for older Linux kernels without MemAvailable: Free + Buffers + Cached
            let free = free_kb.unwrap_or(0);
            let buf = buffers_kb.unwrap_or(0);
            let cache = cached_kb.unwrap_or(0);
            Some(free + buf + cache)
        })
        .unwrap_or(0)
        * 1024;

    let swap_total = swap_total_kb.unwrap_or(0) * 1024;
    let swap_free = swap_free_kb.unwrap_or(0) * 1024;

    Ok(ParsedMemInfo {
        total_bytes: total,
        available_bytes: available,
        swap_total_bytes: swap_total,
        swap_free_bytes: swap_free,
    })
}

fn probe_pidfd_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: pidfd_open has no pointer arguments. A valid return closes it.
        let fd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id() as libc::pid_t, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd as libc::c_int) };
            true
        } else {
            false
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}
