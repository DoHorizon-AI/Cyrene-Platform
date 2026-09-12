// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: kernel/crates/cy-kernel-daemon/src/convert/event.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Event and continuity projections between semantic and Core Proto models.
//!
//! 语义 Event/continuity 模型与 Core Proto 的 projection。
use cy_kernel_api::semantic;
use cy_proto::{core_v1, semantic_v1};
use tonic::Status;

use super::common::{
    semantic_identity_from_proto, semantic_status, timestamp_from_unix_ms,
    to_semantic_proto_identity,
};

pub(crate) fn semantic_operation_event_kind(state: semantic::OperationState) -> &'static str {
    match state {
        semantic::OperationState::Created => "operation.created",
        semantic::OperationState::Pending => "operation.pending",
        semantic::OperationState::Running => "operation.running",
        semantic::OperationState::Succeeded => "operation.succeeded",
        semantic::OperationState::Failed => "operation.failed",
        semantic::OperationState::Cancelling => "operation.cancelling",
        semantic::OperationState::Cancelled => "operation.cancelled",
        semantic::OperationState::Lost => "operation.lost",
    }
}

pub(crate) fn semantic_event_cursor_from_proto(
    cursor: Option<semantic_v1::EventCursor>,
) -> Result<semantic::EventCursor, Status> {
    let cursor = cursor.ok_or_else(|| {
        semantic_status(
            tonic::Code::InvalidArgument,
            "REQUIRED_FIELD_MISSING",
            "event cursor is required",
        )
    })?;
    let cursor = semantic::EventCursor {
        source: semantic_identity_from_proto(cursor.source, "event cursor source")?,
        sequence: cursor.sequence,
    };
    cursor.validate().map_err(|error| {
        semantic_status(
            tonic::Code::InvalidArgument,
            error.reason_code,
            &error.message,
        )
    })?;
    Ok(cursor)
}

pub(crate) fn to_semantic_proto_event(event: &semantic::Event) -> semantic_v1::Event {
    semantic_v1::Event {
        sequence: event.sequence,
        source: Some(to_semantic_proto_identity(&event.source)),
        subject: Some(to_semantic_proto_identity(&event.subject)),
        kind: event.kind.clone(),
        observed_at: Some(timestamp_from_unix_ms(event.observed_at_unix_ms)),
        schema_id: event.schema_id.clone(),
        body: event.body.clone(),
    }
}

#[allow(dead_code)]
pub(crate) fn to_semantic_proto_event_page(page: &semantic::EventPage) -> semantic_v1::EventPage {
    semantic_v1::EventPage {
        source: Some(to_semantic_proto_identity(&page.source)),
        status: match page.status {
            semantic::ReplayStatus::Current => semantic_v1::ReplayStatus::Current,
            semantic::ReplayStatus::Gap => semantic_v1::ReplayStatus::Gap,
            semantic::ReplayStatus::SourceChanged => semantic_v1::ReplayStatus::SourceChanged,
        } as i32,
        events: page.events.iter().map(to_semantic_proto_event).collect(),
        oldest_available_sequence: page.oldest_available_sequence,
        latest_available_sequence: page.latest_available_sequence,
        next_sequence: page.next_sequence,
    }
}

pub(crate) fn to_semantic_proto_event_continuity(
    continuity: &semantic::EventContinuity,
) -> semantic_v1::EventContinuity {
    semantic_v1::EventContinuity {
        source: Some(to_semantic_proto_identity(&continuity.source)),
        status: match continuity.status {
            semantic::ReplayStatus::Current => semantic_v1::ReplayStatus::Current,
            semantic::ReplayStatus::Gap => semantic_v1::ReplayStatus::Gap,
            semantic::ReplayStatus::SourceChanged => semantic_v1::ReplayStatus::SourceChanged,
        } as i32,
        oldest_available_sequence: continuity.oldest_available_sequence,
        latest_available_sequence: continuity.latest_available_sequence,
        next_sequence: continuity.next_sequence,
    }
}

pub(crate) fn operation_event_matches(event: &core_v1::OperationEvent, names: &[String]) -> bool {
    names.is_empty()
        || event
            .operation
            .as_ref()
            .is_some_and(|operation| names.contains(&operation.name))
}

pub(crate) fn runtime_event_kind(event_type: core_v1::RuntimeEventType) -> &'static str {
    match event_type {
        core_v1::RuntimeEventType::InstanceStateChanged => "worker.state.changed",
        core_v1::RuntimeEventType::WatchdogTriggered => "worker.watchdog.triggered",
        core_v1::RuntimeEventType::OomKilled => "worker.oom.killed",
        core_v1::RuntimeEventType::AdapterDegraded => "provider.degraded",
        core_v1::RuntimeEventType::CleanupCompleted => "worker.cleanup.completed",
        core_v1::RuntimeEventType::KernelReconciled => "kernel.reconciled",
        core_v1::RuntimeEventType::Unspecified => "kernel.observation",
    }
}
