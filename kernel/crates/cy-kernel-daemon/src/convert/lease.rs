// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/lease.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use cy_kernel_api::{semantic, CgroupLimits, LeaseState, ResourceLease};
use cy_proto::{core_v1, semantic_v1};
use tonic::Status;

use super::common::{timestamp_from_unix_ms, to_semantic_proto_identity};

pub(crate) fn to_semantic_proto_lease(lease: &ResourceLease) -> semantic_v1::Lease {
    semantic_v1::Lease {
        identity: Some(semantic_v1::Identity {
            id: lease.name.clone(),
            generation: lease.generation,
        }),
        holder: Some(to_semantic_proto_identity(&lease.holder)),
        resources: lease
            .allocations
            .iter()
            .map(|allocation| to_semantic_proto_identity(&allocation.resource))
            .collect(),
        state: match lease.state {
            LeaseState::Active => semantic_v1::LeaseState::Active,
            LeaseState::Releasing => semantic_v1::LeaseState::Releasing,
            LeaseState::Released => semantic_v1::LeaseState::Released,
            LeaseState::Expired => semantic_v1::LeaseState::Expired,
            LeaseState::Revoked => semantic_v1::LeaseState::Revoked,
            LeaseState::Failed | LeaseState::Quarantined => semantic_v1::LeaseState::Failed,
        } as i32,
        fence_token: lease.fence_token,
        expires_at: lease.expires_at_unix_ms.map(timestamp_from_unix_ms),
    }
}

pub(crate) fn to_semantic_proto_contract_lease(lease: &semantic::Lease) -> semantic_v1::Lease {
    semantic_v1::Lease {
        identity: Some(to_semantic_proto_identity(&lease.identity)),
        holder: Some(to_semantic_proto_identity(&lease.holder)),
        resources: lease
            .resources
            .iter()
            .map(to_semantic_proto_identity)
            .collect(),
        state: match lease.state {
            semantic::LeaseState::Active => semantic_v1::LeaseState::Active,
            semantic::LeaseState::Releasing => semantic_v1::LeaseState::Releasing,
            semantic::LeaseState::Released => semantic_v1::LeaseState::Released,
            semantic::LeaseState::Expired => semantic_v1::LeaseState::Expired,
            semantic::LeaseState::Revoked => semantic_v1::LeaseState::Revoked,
            semantic::LeaseState::Failed => semantic_v1::LeaseState::Failed,
        } as i32,
        fence_token: lease.fence_token,
        expires_at: lease.expires_at_unix_ms.map(timestamp_from_unix_ms),
    }
}

pub(crate) fn legacy_holder(
    mutation: Option<&core_v1::MutationContext>,
    fallback: &str,
) -> semantic::Identity {
    let id = mutation
        .and_then(|mutation| mutation.request.as_ref())
        .map(|request| {
            if !request.tenant_id.is_empty() || !request.project_id.is_empty() {
                format!("legacy/{}/{}", request.tenant_id, request.project_id)
            } else if !request.request_id.is_empty() {
                format!("legacy/request/{}", request.request_id)
            } else {
                format!("legacy/{fallback}")
            }
        })
        .unwrap_or_else(|| format!("legacy/{fallback}"));
    semantic::Identity { id, generation: 1 }
}

pub(crate) fn cgroup_limits(
    cpu: Option<&core_v1::CpuRequirements>,
    memory: Option<&core_v1::MemoryRequirements>,
) -> Result<CgroupLimits, Status> {
    if let Some(cpu) = cpu {
        if cpu.limit_millicores > 0
            && cpu.request_millicores > 0
            && cpu.request_millicores > cpu.limit_millicores
        {
            return Err(Status::invalid_argument(
                "cpu request_millicores cannot exceed limit_millicores",
            ));
        }
    }
    if let Some(memory) = memory {
        if memory.limit_bytes > 0
            && memory.request_bytes > 0
            && memory.request_bytes > memory.limit_bytes
        {
            return Err(Status::invalid_argument(
                "memory request_bytes cannot exceed limit_bytes",
            ));
        }
    }
    Ok(CgroupLimits {
        cpu_max_millicores: cpu
            .and_then(|value| (value.limit_millicores > 0).then_some(value.limit_millicores)),
        memory_max_bytes: memory
            .and_then(|value| (value.limit_bytes > 0).then_some(value.limit_bytes)),
        cpuset_cpus: None,
    })
}
