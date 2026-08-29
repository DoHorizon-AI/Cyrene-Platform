//! Device BPF filtering program generation, loading, and device mapper.

use std::path::Path;

#[cfg(target_os = "linux")]
use std::fs;

use cy_kernel_api::{
    DeviceBinding, DeviceMapper, EnforcementMode, EnforcementReport, ProviderError,
};

/// A policy-only mapper retained for callers that need a pre-launch decision.
/// It no longer reports HARD as applied: only `attach_device_bpf_filter` can do that.
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
    use std::{fs::File, os::fd::AsRawFd};
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
    let program = build_device_filter_program(&devices);
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
    let program_fd = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_LOAD,
            &attr,
            std::mem::size_of::<BpfProgLoadAttr>(),
        )
    };
    if program_fd < 0 {
        let verifier_log = String::from_utf8_lossy(&log)
            .trim_matches(char::from(0))
            .trim()
            .to_string();
        let detail = if verifier_log.is_empty() {
            std::io::Error::last_os_error().to_string()
        } else {
            verifier_log
        };
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_BPF_LOAD_FAILED",
            &detail,
        ));
    }
    let cgroup = File::open(cgroup_path).map_err(|error| {
        ProviderError::new("linux-device-bpf", "CGROUP_OPEN_FAILED", &error.to_string())
    })?;
    let attach = BpfProgAttachAttr {
        target_fd: cgroup.as_raw_fd() as u32,
        attach_bpf_fd: program_fd as u32,
        attach_type: BPF_CGROUP_DEVICE,
        attach_flags: 0,
        replace_bpf_fd: 0,
    };
    let attached = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_ATTACH,
            &attach,
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    unsafe {
        libc::close(program_fd as libc::c_int);
    }
    if attached < 0 {
        return Err(ProviderError::new(
            "linux-device-bpf",
            "DEVICE_BPF_ATTACH_FAILED",
            &std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(())
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
            return Err(ProviderError::new(
                "linux-device-bpf",
                "DEVICE_NODE_MISSING",
                &format!("{}: {error}", node.path.display()),
            ))
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
            &node.path.display().to_string(),
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
            &node.path.display().to_string(),
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
        insn(BPF_LDX_W_MEM, 4, 1, 0, 0), // ctx.access_type
        insn(BPF_LDX_W_MEM, 2, 1, 4, 0), // ctx.major
        insn(BPF_LDX_W_MEM, 3, 1, 8, 0), // ctx.minor
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
