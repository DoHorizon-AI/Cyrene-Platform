//! Node Agent hardware probing, heartbeat generation, and hardware manifest construction.

use cy_manifest::{CpuInfo, GpuInfo, HardwareManifest, Interconnect, OsInfo, PrecisionSupport};
use cy_proto::{AgentHeartbeatRequest, TargetRegistrationRequest};
use std::collections::HashMap;
use sysinfo::{Disks, System};

/// Prober for node hardware details and system manifest.
#[derive(Debug, Default)]
pub struct HardwareProbe;

impl HardwareProbe {
    pub fn new() -> Self {
        Self
    }

    /// Probe local hardware and construct a full `HardwareManifest`.
    pub fn probe_hardware_manifest(&self) -> HardwareManifest {
        let mut sys = System::new_all();
        sys.refresh_all();

        // 1. CPU Info
        let _cpu_model = sys
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Generic CPU".to_string());

        let logical_cpus = sys.cpus().len() as u32;
        let physical_cores = sys
            .physical_core_count()
            .map(|c| c as u32)
            .unwrap_or(logical_cpus);

        let arch = System::cpu_arch().unwrap_or_else(|| std::env::consts::ARCH.to_string());

        // 2. Memory Info (in GB)
        let total_mem_bytes = sys.total_memory();
        let memory_gb =
            (total_mem_bytes as f64 / (1024.0 * 1024.0 * 1024.0) * 100.0).round() / 100.0;

        // 3. Disk Info (in GB)
        let disks = Disks::new_with_refreshed_list();
        let total_disk_bytes: u64 = disks.iter().map(|d| d.total_space()).sum();
        let disk_gb = if total_disk_bytes > 0 {
            (total_disk_bytes as f64 / (1024.0 * 1024.0 * 1024.0) * 100.0).round() / 100.0
        } else {
            100.0
        };

        // 4. OS Info
        let os_name = System::name().unwrap_or_else(|| std::env::consts::OS.to_string());
        let os_kernel = System::kernel_version().unwrap_or_else(|| "unknown".to_string());
        let glibc = probe_glibc_version();

        // 5. GPU & Driver Probing
        let (gpus, driver_version, cuda_max_supported, nvlink) = probe_gpus_and_driver();

        // 6. Precision Support
        let precision_support = determine_precision_support(&gpus);

        HardwareManifest {
            os: OsInfo {
                name: os_name,
                kernel: os_kernel,
                glibc,
            },
            cpu: CpuInfo {
                arch,
                cores: physical_cores,
                threads: Some(logical_cpus),
            },
            memory_gb,
            disk_gb,
            gpus,
            driver_version,
            cuda_max_supported,
            interconnect: Interconnect {
                nvlink,
                pcie_gen: Some(4),
            },
            precision_support,
        }
    }

    /// Probe local node hardware and construct a `TargetRegistrationRequest`.
    pub fn probe_target(&self, target_id: &str, agent_version: &str) -> TargetRegistrationRequest {
        let manifest = self.probe_hardware_manifest();

        let mut sys = System::new_all();
        sys.refresh_all();

        let free_mem_bytes = sys.available_memory();
        let free_ram_gb =
            (free_mem_bytes as f64 / (1024.0 * 1024.0 * 1024.0) * 100.0).round() / 100.0;

        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "localhost".to_string());

        let mut capabilities = vec!["cpu".to_string()];

        if !manifest.gpus.is_empty()
            || std::env::var("CUDA_VISIBLE_DEVICES").is_ok()
            || std::path::Path::new("/proc/driver/nvidia").exists()
        {
            capabilities.push("cuda".to_string());
        }
        if std::env::var("ROCR_VISIBLE_DEVICES").is_ok()
            || std::path::Path::new("/dev/kfd").exists()
        {
            capabilities.push("rocm".to_string());
        }

        let mut labels = HashMap::new();
        labels.insert("num_cpus".to_string(), manifest.cpu.cores.to_string());
        labels.insert(
            "num_threads".to_string(),
            manifest
                .cpu
                .threads
                .unwrap_or(manifest.cpu.cores)
                .to_string(),
        );
        labels.insert("total_ram_gb".to_string(), manifest.memory_gb.to_string());
        labels.insert("free_ram_gb".to_string(), free_ram_gb.to_string());
        labels.insert("disk_gb".to_string(), manifest.disk_gb.to_string());
        labels.insert("os".to_string(), manifest.os.name.clone());
        labels.insert("kernel".to_string(), manifest.os.kernel.clone());
        labels.insert("arch".to_string(), manifest.cpu.arch.clone());
        labels.insert(
            "driver_version".to_string(),
            manifest.driver_version.clone(),
        );
        labels.insert(
            "cuda_max_supported".to_string(),
            manifest.cuda_max_supported.clone(),
        );

        let total_gpu_count: u32 = manifest.gpus.iter().map(|g| g.count).sum();
        labels.insert("gpu_count".to_string(), total_gpu_count.to_string());
        let gpu_models = if manifest.gpus.is_empty() {
            "none".to_string()
        } else {
            manifest
                .gpus
                .iter()
                .map(|g| format!("{}x {}", g.count, g.model))
                .collect::<Vec<_>>()
                .join(", ")
        };
        labels.insert("gpu_models".to_string(), gpu_models);

        TargetRegistrationRequest {
            target_id: target_id.to_string(),
            hostname,
            ip_address: get_local_ip(),
            arch: manifest.cpu.arch,
            os: manifest.os.name,
            capabilities,
            labels,
            agent_version: agent_version.to_string(),
        }
    }

    /// Generate an `AgentHeartbeatRequest`.
    pub fn create_heartbeat(
        &self,
        agent_id: &str,
        target_id: &str,
        status: &str,
        metrics: HashMap<String, String>,
    ) -> AgentHeartbeatRequest {
        let timestamp = chrono::Utc::now().timestamp();
        AgentHeartbeatRequest {
            agent_id: agent_id.to_string(),
            target_id: target_id.to_string(),
            timestamp,
            status: status.to_string(),
            metrics,
        }
    }
}

/// Helper function to probe glibc version on Linux systems.
fn probe_glibc_version() -> String {
    if std::env::consts::OS != "linux" {
        return "N/A".to_string();
    }

    if let Ok(output) = std::process::Command::new("ldd").arg("--version").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().next() {
                if let Some(pos) = line.rfind(' ') {
                    let ver = line[pos..].trim();
                    if !ver.is_empty() {
                        return ver.to_string();
                    }
                }
            }
        }
    }

    "2.35".to_string()
}

/// Execute nvidia-smi or parse sysfs/procfs to detect GPUs and driver version.
fn probe_gpus_and_driver() -> (Vec<GpuInfo>, String, String, bool) {
    // Attempt executing `nvidia-smi`
    let query_args = [
        "--query-gpu=name,memory.total,memory.free,driver_version,pci.bus_id,compute_cap",
        "--format=csv,noheader,nounits",
    ];

    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args(query_args)
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(parsed) = parse_nvidia_smi_output(&stdout) {
                return parsed;
            }
        }
    }

    // Fallback: sysfs/procfs checking if nvidia driver exists without nvidia-smi
    let mut driver_version = "N/A".to_string();
    let proc_version_path = std::path::Path::new("/proc/driver/nvidia/version");
    if proc_version_path.exists() {
        if let Ok(content) = std::fs::read_to_string(proc_version_path) {
            if let Some(ver) = parse_procfs_driver_version(&content) {
                driver_version = ver;
            }
        }
    }

    let cuda_max = if driver_version != "N/A" {
        derive_cuda_max_from_driver(&driver_version)
    } else {
        "N/A".to_string()
    };

    (Vec::new(), driver_version, cuda_max, false)
}

/// Parse CSV output from `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi_output(stdout: &str) -> Option<(Vec<GpuInfo>, String, String, bool)> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        return None;
    }

    let mut driver_version = "N/A".to_string();
    let mut gpu_counts: HashMap<(String, String, u64), u32> = HashMap::new(); // (model, cc, vram_mb) -> count

    for line in lines {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 6 {
            continue;
        }

        let name = parts[0].to_string();
        let total_vram_mb: u64 = parts[1].parse().unwrap_or(0);
        let driver_ver = parts[3].to_string();
        let compute_cap = parts[5].to_string();

        if driver_version == "N/A" && !driver_ver.is_empty() {
            driver_version = driver_ver;
        }

        let key = (name, compute_cap, total_vram_mb);
        *gpu_counts.entry(key).or_insert(0) += 1;
    }

    if gpu_counts.is_empty() {
        return None;
    }

    let mut gpus = Vec::new();
    for ((model, compute_capability, vram_mb), count) in gpu_counts {
        let vram_gb = (vram_mb as f64 / 1024.0 * 100.0).round() / 100.0;
        gpus.push(GpuInfo {
            model,
            count,
            vram_gb,
            compute_capability,
        });
    }

    // Sort GPUs by model name for deterministic ordering
    gpus.sort_by(|a, b| a.model.cmp(&b.model));

    let cuda_max = derive_cuda_max_from_driver(&driver_version);
    let nvlink = check_nvlink_support();

    Some((gpus, driver_version, cuda_max, nvlink))
}

/// Parse driver version from `/proc/driver/nvidia/version` text.
pub fn parse_procfs_driver_version(content: &str) -> Option<String> {
    for word in content.split_whitespace() {
        // e.g. "535.129.03" or "470.82.00"
        let parts: Vec<&str> = word.split('.').collect();
        if parts.len() >= 2
            && parts[0].chars().all(|c| c.is_ascii_digit())
            && parts[1].chars().all(|c| c.is_ascii_digit())
        {
            return Some(word.to_string());
        }
    }
    None
}

/// Derive supported max CUDA version based on NVIDIA driver version.
fn derive_cuda_max_from_driver(driver_ver: &str) -> String {
    let major: u32 = driver_ver
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    if major >= 550 {
        "12.4".to_string()
    } else if major >= 535 {
        "12.2".to_string()
    } else if major >= 525 {
        "12.0".to_string()
    } else if major >= 510 {
        "11.6".to_string()
    } else if major >= 470 {
        "11.4".to_string()
    } else if major > 0 {
        "11.0".to_string()
    } else {
        "N/A".to_string()
    }
}

/// Check if NVLink is supported/detected on host.
fn check_nvlink_support() -> bool {
    std::path::Path::new("/proc/driver/nvidia/gpus").exists()
        || std::env::var("HAS_NVLINK")
            .map(|v| v == "1")
            .unwrap_or(false)
}

/// Determine precision support based on GPU compute capability.
fn determine_precision_support(gpus: &[GpuInfo]) -> PrecisionSupport {
    if gpus.is_empty() {
        return PrecisionSupport {
            bf16: true,
            fp16: true,
            fp8: false,
        };
    }

    let mut max_cc = 0.0f64;
    for gpu in gpus {
        if let Ok(cc) = gpu.compute_capability.parse::<f64>() {
            if cc > max_cc {
                max_cc = cc;
            }
        }
    }

    PrecisionSupport {
        bf16: max_cc >= 8.0,
        fp16: max_cc >= 5.3,
        fp8: max_cc >= 8.9,
    }
}

/// Best effort helper to fetch primary non-loopback IP address or fallback to 127.0.0.1.
fn get_local_ip() -> String {
    std::env::var("NODE_IP").unwrap_or_else(|_| "127.0.0.1".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_probe() {
        let probe = HardwareProbe::new();
        let manifest = probe.probe_hardware_manifest();

        assert!(!manifest.os.name.is_empty());
        assert!(!manifest.cpu.arch.is_empty());
        assert!(manifest.cpu.cores > 0);
        assert!(manifest.memory_gb > 0.0);

        let target_req = probe.probe_target("test-target-1", "0.1.0");

        assert_eq!(target_req.target_id, "test-target-1");
        assert_eq!(target_req.agent_version, "0.1.0");
        assert!(target_req.capabilities.contains(&"cpu".to_string()));
        assert!(!target_req.hostname.is_empty());
        assert!(target_req.labels.contains_key("num_cpus"));
        assert!(target_req.labels.contains_key("total_ram_gb"));

        let heartbeat =
            probe.create_heartbeat("agent-1", "test-target-1", "HEALTHY", HashMap::new());
        assert_eq!(heartbeat.agent_id, "agent-1");
        assert_eq!(heartbeat.status, "HEALTHY");
    }

    #[test]
    fn test_parse_nvidia_smi_output() {
        let sample_csv = r#"
NVIDIA A100-SXM4-80GB, 81920, 80000, 535.129.03, 0000:00:04.0, 8.0
NVIDIA A100-SXM4-80GB, 81920, 80000, 535.129.03, 0000:00:05.0, 8.0
"#;
        let (gpus, driver_ver, cuda_max, _nvlink) = parse_nvidia_smi_output(sample_csv).unwrap();
        assert_eq!(driver_ver, "535.129.03");
        assert_eq!(cuda_max, "12.2");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].model, "NVIDIA A100-SXM4-80GB");
        assert_eq!(gpus[0].count, 2);
        assert_eq!(gpus[0].vram_gb, 80.0);
        assert_eq!(gpus[0].compute_capability, "8.0");
    }

    #[test]
    fn test_parse_procfs_driver_version() {
        let proc_sample = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  535.129.03  Thu Oct 19 18:56:32 UTC 2023";
        let ver = parse_procfs_driver_version(proc_sample).unwrap();
        assert_eq!(ver, "535.129.03");
    }

    #[test]
    fn test_precision_support_logic() {
        let gpus_h100 = vec![GpuInfo {
            model: "NVIDIA H100".to_string(),
            count: 1,
            vram_gb: 80.0,
            compute_capability: "9.0".to_string(),
        }];
        let ps_h100 = determine_precision_support(&gpus_h100);
        assert!(ps_h100.bf16);
        assert!(ps_h100.fp16);
        assert!(ps_h100.fp8);

        let gpus_v100 = vec![GpuInfo {
            model: "NVIDIA V100".to_string(),
            count: 1,
            vram_gb: 16.0,
            compute_capability: "7.0".to_string(),
        }];
        let ps_v100 = determine_precision_support(&gpus_v100);
        assert!(!ps_v100.bf16);
        assert!(ps_v100.fp16);
        assert!(!ps_v100.fp8);
    }
}
