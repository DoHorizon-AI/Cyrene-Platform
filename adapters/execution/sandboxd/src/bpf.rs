// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/bpf.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Device BPF filtering program generation, loading, and device mapper.

use std::path::Path;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;

use cy_kernel_api::{
    DeviceBinding, DeviceMapper, EnforcementMode, EnforcementReport, ProviderError,
};

/// A policy-only mapper retained for callers that need a pre-launch decision.
/// It no longer reports HARD as applied: only `attach_device_bpf_filter` can do that.
/// 中文：仅用于策略判定的映射器，为需要在启动前作出决策的调用方保留。它不再报告 HARD 策略已应用；只有 `attach_device_bpf_filter` 可以作出此报告。
#[derive(Debug, Clone, Copy)]
pub struct LinuxDeviceMapper {
    pub device_bpf_enabled: bool,
}

impl DeviceMapper for LinuxDeviceMapper {
    fn enforce(
        &self,
        binding: &DeviceBinding,
        requested: EnforcementMode,
    ) -> Result<EnforcementReport, ProviderError> {
        if requested == EnforcementMode::Hard && !self.device_bpf_enabled {
            return Err(ProviderError::new(
                "linux-device-bpf",
                "HARD_ENFORCEMENT_UNAVAILABLE",
                "device BPF is disabled",
            ));
        }
        Ok(EnforcementReport {
            resource_kind: "accelerator".to_string(),
            mode: if requested == EnforcementMode::Hard {
                EnforcementMode::ObserveOnly
            } else {
                requested
            },
            adapter_id: binding.adapter_id.clone(),
            reason_code: if requested == EnforcementMode::Hard {
                "HARD_ENFORCEMENT_PENDING_CGROUP_ATTACH".to_string()
            } else {
                "NON_HARD_POLICY".to_string()
            },
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn attach_device_bpf_filter(
    cgroup_path: &Path,
    binding: &DeviceBinding,
) -> Result<(), ProviderError> {
    use std::os::fd::AsRawFd;

    let devices = device_rules(binding)?;
    let program = build_device_filter_program(&devices);
    let program_fd = load_device_filter_program(&program)?;
    attach_program_to_cgroup(cgroup_path, program_fd.as_raw_fd())
}

#[cfg(target_os = "linux")]
pub(crate) fn probe_device_bpf_attach(
    cgroup_path: &Path,
    binding: Option<&DeviceBinding>,
) -> Result<(), ProviderError> {
    use std::os::fd::AsRawFd;

    let program = if let Some(binding) = binding {
        build_device_filter_program(&device_rules(binding)?)
    } else {
        // A global node probe has no device binding. Use a minimal allow
        // program so it proves program load and cgroup attach permissions
        // without granting access to a process in the empty probe cgroup.
        vec![
            insn(BPF_ALU64_MOV_K, 0, 0, 0, 1),
            insn(BPF_JMP_EXIT, 0, 0, 0, 0),
        ]
    };
    let program_fd = load_device_filter_program(&program)?;
    let cgroup = std::fs::File::open(cgroup_path)
        .map_err(|error| cgroup_open_failure(cgroup_path, error))?;
    let attach = BpfProgAttachAttr {
        target_fd: cgroup.as_raw_fd() as u32,
        attach_bpf_fd: program_fd.as_raw_fd() as u32,
        attach_type: BPF_CGROUP_DEVICE,
        attach_flags: 0,
        replace_bpf_fd: 0,
    };
    // SAFETY: both descriptors are live, and `attach` remains valid until
    // the synchronous syscall returns.
    // 中文：两个描述符均处于有效期内，且属性结构在同步系统调用期间保持有效。
    let attached = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_ATTACH,
            &attach,
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    if attached < 0 {
        let error = std::io::Error::last_os_error();
        return Err(attach_failure(error));
    }

    // The probe cgroup is private, empty, and removed by the caller. Detach
    // the exact program before returning so the probe leaves no attached
    // filter behind even if directory cleanup is delayed.
    // 中文：探测 cgroup 是隔离且空的；返回前先卸载当前程序，确保清理延迟时也不遗留过滤器。
    const BPF_PROG_DETACH: libc::c_uint = 9;
    // SAFETY: the same live target/program descriptors and attach attributes
    // identify the program attached immediately above.
    // 中文：同一组有效目标/程序描述符及属性对应刚刚附加的程序。
    let detached = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_DETACH,
            &attach,
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    if detached < 0 {
        return Err(detach_failure(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn probe_device_bpf_attach(
    _cgroup_path: &Path,
    _binding: Option<&DeviceBinding>,
) -> Result<(), ProviderError> {
    Err(ProviderError::new(
        "linux-device-bpf",
        "DEVICE_BPF_UNSUPPORTED",
        "cgroup device BPF requires Linux; HARD GPU isolation is unavailable",
    ))
}

#[cfg(target_os = "linux")]
fn device_rules(binding: &DeviceBinding) -> Result<Vec<DeviceRule>, ProviderError> {
    let mut devices = Vec::new();
    for node in &binding.nodes {
        if let Some(rule) = device_rule(node)? {
            devices.push(rule);
        }
    }
    if devices.is_empty() {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_RULES_EMPTY",
            "HARD binding contains no valid device nodes",
        ));
    }
    Ok(devices)
}

#[cfg(target_os = "linux")]
fn load_device_filter_program(program: &[BpfInsn]) -> Result<std::os::fd::OwnedFd, ProviderError> {
    use std::os::fd::OwnedFd;

    let license = b"GPL\0";
    let mut log = vec![0_u8; 16 * 1024];
    let attr = BpfProgLoadAttr {
        prog_type: BPF_PROG_TYPE_CGROUP_DEVICE,
        insn_cnt: program.len() as u32,
        insns: program.as_ptr() as u64,
        license: license.as_ptr() as u64,
        log_level: 1,
        log_size: log.len() as u32,
        log_buf: log.as_mut_ptr() as u64,
        kern_version: 0,
        prog_flags: 0,
        prog_name: *b"cyrene_devflt\0\0\0",
        prog_ifindex: 0,
        expected_attach_type: BPF_CGROUP_DEVICE,
    };
    // SAFETY: `attr` and the instruction, license, and log buffers remain
    // alive for the duration of the synchronous `bpf(2)` call.
    // 中文：`bpf(2)` 同步调用期间，属性及其引用的缓冲区均保持有效。
    let program_fd = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_LOAD,
            &attr,
            std::mem::size_of::<BpfProgLoadAttr>(),
        )
    };
    if program_fd < 0 {
        let err = std::io::Error::last_os_error();
        let verifier_log = String::from_utf8_lossy(&log)
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        return Err(load_failure(err, &verifier_log));
    }
    // SAFETY: a nonnegative descriptor was returned by a successful
    // BPF_PROG_LOAD, transferring its ownership to this `OwnedFd`.
    // 中文：成功的 BPF_PROG_LOAD 返回了有效描述符，由此 `OwnedFd` 接管其所有权。
    Ok(unsafe { OwnedFd::from_raw_fd(program_fd as libc::c_int) })
}

#[cfg(target_os = "linux")]
fn attach_program_to_cgroup(
    cgroup_path: &Path,
    program_fd: std::os::fd::RawFd,
) -> Result<(), ProviderError> {
    use std::os::fd::AsRawFd;

    let cgroup = std::fs::File::open(cgroup_path)
        .map_err(|error| cgroup_open_failure(cgroup_path, error))?;
    let attach = BpfProgAttachAttr {
        target_fd: cgroup.as_raw_fd() as u32,
        attach_bpf_fd: program_fd as u32,
        attach_type: BPF_CGROUP_DEVICE,
        attach_flags: 0,
        replace_bpf_fd: 0,
    };
    // SAFETY: both descriptors are live, and `attach` remains valid until the
    // synchronous syscall returns.
    // 中文：两个文件描述符均处于有效期内，且属性结构在同步系统调用期间保持有效。
    let attached = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_ATTACH,
            &attach,
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    if attached < 0 {
        let error = std::io::Error::last_os_error();
        return Err(attach_failure(error));
    }
    Ok(())
}

/// Converts a failed program load into a stable, actionable public error.
///
/// Permission denials are separated from unsupported kernels and verifier
/// failures so Kernel and Yield can present a predictable blocking reason.
/// 中文：区分权限拒绝、不支持的内核与验证器失败，供 Kernel 和 Yield 稳定识别阻塞原因。
#[cfg(target_os = "linux")]
fn load_failure(error: std::io::Error, verifier_log: &str) -> ProviderError {
    let reason_code = match error.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => "DEVICE_BPF_PERMISSION_DENIED",
        Some(libc::EOPNOTSUPP) => "DEVICE_BPF_UNSUPPORTED",
        _ => "DEVICE_BPF_LOAD_FAILED",
    };
    let mut detail = format!("loading cgroup device BPF program failed: {error}");
    if !verifier_log.is_empty() {
        detail.push_str("; verifier: ");
        detail.push_str(verifier_log);
    }
    if reason_code == "DEVICE_BPF_PERMISSION_DENIED" {
        detail.push_str(
            "; check CAP_BPF/CAP_SYS_ADMIN, the cgroup delegation, unprivileged BPF policy, and LSM restrictions; HARD GPU isolation remains unavailable until the host permits BPF loading",
        );
    } else if reason_code == "DEVICE_BPF_UNSUPPORTED" {
        detail.push_str(
            "; this kernel does not support cgroup device BPF; HARD GPU isolation requires a compatible Linux kernel",
        );
    }
    ProviderError::new("linux-device-bpf", reason_code, &detail)
}

/// Converts a failed cgroup attach into a stable, actionable public error.
/// 中文：将 cgroup 挂载失败映射为稳定且可操作的公开错误。
#[cfg(target_os = "linux")]
fn attach_failure(error: std::io::Error) -> ProviderError {
    let reason_code = match error.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => "DEVICE_BPF_PERMISSION_DENIED",
        Some(libc::EOPNOTSUPP) => "DEVICE_BPF_UNSUPPORTED",
        _ => "DEVICE_BPF_ATTACH_FAILED",
    };
    let detail = if reason_code == "DEVICE_BPF_PERMISSION_DENIED" {
        format!(
            "attaching cgroup device BPF filter failed: {error}; check CAP_BPF/CAP_SYS_ADMIN, write access to the delegated cgroup, unprivileged BPF policy, and LSM restrictions; HARD GPU launch was rejected"
        )
    } else if reason_code == "DEVICE_BPF_UNSUPPORTED" {
        format!(
            "attaching cgroup device BPF filter failed: {error}; the cgroup does not support device BPF; HARD GPU launch was rejected"
        )
    } else {
        format!("attaching cgroup device BPF filter failed: {error}")
    };
    ProviderError::new("linux-device-bpf", reason_code, &detail)
}

/// Converts a failed detach of a temporary attach probe into a stable error.
/// 中文：将临时附加探测的卸载失败转换为稳定错误。
#[cfg(target_os = "linux")]
fn detach_failure(error: std::io::Error) -> ProviderError {
    let reason_code = match error.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => "DEVICE_BPF_PERMISSION_DENIED",
        Some(libc::EOPNOTSUPP) => "DEVICE_BPF_UNSUPPORTED",
        _ => "DEVICE_BPF_DETACH_FAILED",
    };
    ProviderError::new(
        "linux-device-bpf",
        reason_code,
        &format!(
            "detaching cgroup device BPF probe failed: {error}; HARD GPU isolation cannot be certified"
        ),
    )
}

/// Preserves cgroup-open permission failures as an isolation blocker.
/// 中文：将打开 cgroup 时的权限错误明确报告为隔离阻塞原因。
#[cfg(target_os = "linux")]
fn cgroup_open_failure(path: &Path, error: std::io::Error) -> ProviderError {
    let denied = matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES));
    let reason_code = if denied {
        "DEVICE_BPF_PERMISSION_DENIED"
    } else {
        "CGROUP_OPEN_FAILED"
    };
    let detail = if denied {
        format!(
            "opening cgroup {} for device BPF failed: {error}; check ownership and permissions on the delegated cgroup subtree; HARD GPU isolation remains unavailable",
            path.display()
        )
    } else {
        format!(
            "opening cgroup {} for device BPF failed: {error}",
            path.display()
        )
    };
    ProviderError::new("linux-device-bpf", reason_code, &detail)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn attach_device_bpf_filter(
    _cgroup_path: &Path,
    _binding: &DeviceBinding,
) -> Result<(), ProviderError> {
    Err(ProviderError::new(
        "linux-device-bpf",
        "HARD_ENFORCEMENT_UNAVAILABLE",
        "cgroup device BPF requires Linux",
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn device_rule(
    node: &cy_kernel_api::DeviceNode,
) -> Result<Option<DeviceRule>, ProviderError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match fs::metadata(&node.path) {
        Ok(metadata) => metadata,
        Err(_error) if !node.required => return Ok(None),
        Err(error) => {
            let (reason_code, diagnosis) = match error.raw_os_error() {
                Some(libc::EPERM | libc::EACCES) => (
                    "DEVICE_NODE_PERMISSION_DENIED",
                    "check sandboxd access to the required device node",
                ),
                Some(libc::ENOENT) => (
                    "DEVICE_NODE_MISSING",
                    "the required device node does not exist",
                ),
                _ => (
                    "DEVICE_NODE_STAT_FAILED",
                    "the required device node could not be inspected",
                ),
            };
            return Err(ProviderError::new(
                "linux-device-bpf",
                reason_code,
                &format!("{}: {diagnosis}: {error}", node.path.display()),
            ));
        }
    };
    let kind = if metadata.file_type().is_char_device() {
        BPF_DEVCG_DEV_CHAR
    } else if metadata.file_type().is_block_device() {
        BPF_DEVCG_DEV_BLOCK
    } else {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_NODE_TYPE_INVALID",
            &format!(
                "{}: expected a character or block device node",
                node.path.display()
            ),
        ));
    };
    let major = (metadata.rdev() >> 8) & 0x0fff;
    let minor = (metadata.rdev() & 0xff) | ((metadata.rdev() >> 12) & 0x0fff00);
    if node.major.is_some_and(|expected| expected != major as u32)
        || node.minor.is_some_and(|expected| expected != minor as u32)
    {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_NODE_IDENTITY_CHANGED",
            &format!(
                "{}: expected major/minor {:?}/{:?}, observed {major}/{minor}",
                node.path.display(),
                node.major,
                node.minor
            ),
        ));
    }
    Ok(Some(DeviceRule {
        kind,
        major: major as u32,
        minor: minor as u32,
    }))
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeviceRule {
    pub(crate) kind: u32,
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct BpfInsn {
    pub(crate) code: u8,
    pub(crate) dst_src: u8,
    pub(crate) off: i16,
    pub(crate) imm: i32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
pub(crate) struct BpfProgLoadAttr {
    pub(crate) prog_type: u32,
    pub(crate) insn_cnt: u32,
    pub(crate) insns: u64,
    pub(crate) license: u64,
    pub(crate) log_level: u32,
    pub(crate) log_size: u32,
    pub(crate) log_buf: u64,
    pub(crate) kern_version: u32,
    pub(crate) prog_flags: u32,
    pub(crate) prog_name: [u8; 16],
    pub(crate) prog_ifindex: u32,
    pub(crate) expected_attach_type: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
pub(crate) struct BpfProgAttachAttr {
    pub(crate) target_fd: u32,
    pub(crate) attach_bpf_fd: u32,
    pub(crate) attach_type: u32,
    pub(crate) attach_flags: u32,
    pub(crate) replace_bpf_fd: u32,
}

#[cfg(target_os = "linux")]
pub(crate) fn build_device_filter_program(rules: &[DeviceRule]) -> Vec<BpfInsn> {
    let mut program = vec![
        insn(BPF_LDX_W_MEM, 4, 1, 0, 0), // ctx.access_type | 中文：上下文中的访问类型
        insn(BPF_LDX_W_MEM, 2, 1, 4, 0), // ctx.major | 中文：设备主编号
        insn(BPF_LDX_W_MEM, 3, 1, 8, 0), // ctx.minor | 中文：设备次编号
        insn(BPF_ALU64_AND_K, 4, 0, 0, 0xffff_0000_u32 as i32),
    ];
    let mut skip_indices = Vec::new();
    for rule in rules {
        let start = program.len();
        program.push(insn(BPF_JMP_JEQ_K, 4, 0, 1, rule.kind as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_JMP_JEQ_K, 2, 0, 1, rule.major as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_JMP_JEQ_K, 3, 0, 1, rule.minor as i32));
        skip_indices.push(program.len());
        program.push(insn(BPF_JMP_A, 0, 0, 0, 0));
        program.push(insn(BPF_ALU64_MOV_K, 0, 0, 0, 1));
        program.push(insn(BPF_JMP_EXIT, 0, 0, 0, 0));
        let next = program.len();
        for index in skip_indices.drain(..) {
            program[index].off = (next - index - 1) as i16;
        }
        debug_assert_eq!(start + 8, next);
    }
    program.push(insn(BPF_ALU64_MOV_K, 0, 0, 0, 0));
    program.push(insn(BPF_JMP_EXIT, 0, 0, 0, 0));
    program
}

#[cfg(target_os = "linux")]
pub(crate) fn insn(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> BpfInsn {
    BpfInsn {
        code,
        dst_src: dst | (src << 4),
        off,
        imm,
    }
}

#[cfg(target_os = "linux")]
pub(crate) const BPF_PROG_LOAD: libc::c_long = 5;
#[cfg(target_os = "linux")]
pub(crate) const BPF_PROG_ATTACH: libc::c_long = 8;
#[cfg(target_os = "linux")]
pub(crate) const BPF_PROG_TYPE_CGROUP_DEVICE: u32 = 15;
#[cfg(target_os = "linux")]
pub(crate) const BPF_CGROUP_DEVICE: u32 = 6;
#[cfg(target_os = "linux")]
pub(crate) const BPF_DEVCG_DEV_BLOCK: u32 = 1 << 16;
#[cfg(target_os = "linux")]
pub(crate) const BPF_DEVCG_DEV_CHAR: u32 = 2 << 16;
#[cfg(target_os = "linux")]
pub(crate) const BPF_LDX_W_MEM: u8 = 0x61;
#[cfg(target_os = "linux")]
pub(crate) const BPF_ALU64_AND_K: u8 = 0x57;
#[cfg(target_os = "linux")]
pub(crate) const BPF_ALU64_MOV_K: u8 = 0xb7;
#[cfg(target_os = "linux")]
pub(crate) const BPF_JMP_JEQ_K: u8 = 0x15;
#[cfg(target_os = "linux")]
pub(crate) const BPF_JMP_A: u8 = 0x05;
#[cfg(target_os = "linux")]
pub(crate) const BPF_JMP_EXIT: u8 = 0x95;

#[cfg(all(test, target_os = "linux"))]
mod bpf_error_tests {
    use super::{attach_failure, load_failure};

    #[test]
    fn permission_errors_have_stable_codes_and_no_dev_mode_bypass_advice() {
        let load = load_failure(std::io::Error::from_raw_os_error(libc::EPERM), "");
        assert_eq!(load.reason_code, "DEVICE_BPF_PERMISSION_DENIED");
        assert!(load.message.contains("CAP_BPF/CAP_SYS_ADMIN"));
        assert!(load.message.contains("LSM"));
        assert!(!load.message.contains("--dev-mode"));

        let attach = attach_failure(std::io::Error::from_raw_os_error(libc::EACCES));
        assert_eq!(attach.reason_code, "DEVICE_BPF_PERMISSION_DENIED");
        assert!(attach.message.contains("HARD GPU launch was rejected"));
    }

    #[test]
    fn load_errors_preserve_verifier_diagnostics() {
        let error = load_failure(
            std::io::Error::from_raw_os_error(libc::EINVAL),
            "invalid program type",
        );
        assert_eq!(error.reason_code, "DEVICE_BPF_LOAD_FAILED");
        assert!(error.message.contains("invalid program type"));
    }
}
