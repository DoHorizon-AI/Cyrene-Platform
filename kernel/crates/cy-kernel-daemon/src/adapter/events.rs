//! KernelServiceAdapter 事件分发、历史追溯与运行时日志沉淀。

use std::sync::atomic::Ordering;

use cy_kernel_api::{
    semantic, CleanupReport, NamespaceId, ProviderError, RuntimeJournalEvent, RuntimeJournalRecord,
    RuntimeProcessEvidence,
};
use cy_proto::core_v1;

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{now_timestamp, runtime_event_kind},
};

impl KernelServiceAdapter {
    pub(crate) fn remember_operation(&self, operation: core_v1::Operation) -> core_v1::Operation {
        let mut operations = self.operations.lock().expect("operation lock poisoned");
        operations.insert(operation.name.clone(), operation.clone());
        drop(operations);
        self.publish_operation_event(operation.clone());
        operation
    }

    pub(crate) fn publish_operation_event(&self, operation: core_v1::Operation) {
        self.publish_event(core_v1::OperationEvent {
            event_id: String::new(),
            resume_token: String::new(),
            sequence_number: 0,
            operation: Some(operation),
            runtime_event: None,
        });
    }

    pub fn publish_runtime_event(
        &self,
        event_type: core_v1::RuntimeEventType,
        target_resource_name: impl Into<String>,
        reason_code: impl Into<String>,
        summary: impl Into<String>,
    ) {
        self.publish_runtime_event_in(
            &NamespaceId::default(),
            event_type,
            target_resource_name,
            reason_code,
            summary,
        );
    }

    pub(crate) fn publish_runtime_event_in(
        &self,
        namespace: &NamespaceId,
        event_type: core_v1::RuntimeEventType,
        target_resource_name: impl Into<String>,
        reason_code: impl Into<String>,
        summary: impl Into<String>,
    ) {
        let target_resource_name = target_resource_name.into();
        let reason_code = reason_code.into();
        let summary = summary.into();
        self.publish_event(core_v1::OperationEvent {
            event_id: String::new(),
            resume_token: String::new(),
            sequence_number: 0,
            operation: None,
            runtime_event: Some(core_v1::RuntimeEvent {
                r#type: event_type as i32,
                target_resource_name: target_resource_name.clone(),
                reason_code: reason_code.clone(),
                summary: summary.clone(),
                observed_at: Some(now_timestamp()),
            }),
        });
        self.authority.publish_semantic_event_in(
            namespace,
            semantic::Identity {
                id: target_resource_name,
                generation: 1,
            },
            runtime_event_kind(event_type),
            "cyrene.runtime.v1",
            format!("{reason_code}:{summary}").into_bytes(),
        );
    }

    pub(crate) fn publish_event(&self, mut event: core_v1::OperationEvent) {
        let sequence_number = self.next_event_sequence.fetch_add(1, Ordering::Relaxed);
        event.event_id = format!("operation-event-{sequence_number}");
        event.resume_token = sequence_number.to_string();
        event.sequence_number = sequence_number;
        let mut history = self
            .operation_events
            .lock()
            .expect("operation event history lock poisoned");
        if history.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(event.clone());
        drop(history);
        let _ = self.operation_event_sender.send(event);
    }

    #[cfg(test)]
    pub(crate) fn semantic_event_source(&self) -> semantic::Identity {
        self.authority.semantic_event_source()
    }

    #[cfg(test)]
    pub(crate) fn publish_semantic_event(
        &self,
        subject: semantic::Identity,
        kind: impl Into<String>,
        schema_id: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) {
        self.authority
            .publish_semantic_event(subject, kind, schema_id, body);
    }

    /// Persists a runtime lifecycle record to the durable journal.
    ///
    /// For lease reservation/release this is the authoritative step: callers
    /// must treat a returned `Err` as a hard failure (the fence record was not
    /// durably reserved) and roll back or fail-closed accordingly, never
    /// letting a lease become externally visible without its persisted fence.
    pub(crate) fn record_runtime(
        &self,
        event: RuntimeJournalEvent,
        instance_name: Option<&str>,
        lease: Option<&core_v1::ResourceLeaseRef>,
        reason_code: &str,
    ) -> Result<(), ProviderError> {
        self.runtime_journal.append(RuntimeJournalRecord {
            event,
            node_id: self.daemon.node_id.clone(),
            node_epoch: self.daemon.node_epoch,
            instance_name: instance_name.map(str::to_owned),
            lease_name: lease.map(|lease| lease.lease_name.clone()),
            fence_token: lease.map(|lease| lease.fence_token),
            reason_code: reason_code.to_string(),
            runtime_evidence: None,
        })
    }

    /// Records post-launch local runtime identity before exposing a process to
    /// callers, workers, or semantic event subscribers.
    pub(crate) fn record_runtime_launch(
        &self,
        instance_name: &str,
        lease: &core_v1::ResourceLeaseRef,
        evidence: RuntimeProcessEvidence,
    ) -> Result<(), ProviderError> {
        self.runtime_journal.append(RuntimeJournalRecord {
            event: RuntimeJournalEvent::InstanceLaunched,
            node_id: self.daemon.node_id.clone(),
            node_epoch: self.daemon.node_epoch,
            instance_name: Some(instance_name.to_string()),
            lease_name: Some(lease.lease_name.clone()),
            fence_token: Some(lease.fence_token),
            reason_code: "WORKER_LAUNCHED".to_string(),
            runtime_evidence: Some(evidence),
        })
    }

    pub(crate) fn publish_cleanup_events(&self, target: &str, report: &CleanupReport) {
        if report.oom_killed {
            self.publish_runtime_event(
                core_v1::RuntimeEventType::OomKilled,
                target,
                "OOM_KILLED",
                "sandbox telemetry recorded an OOM kill during cleanup",
            );
        }
        if report.complete {
            self.publish_runtime_event(
                core_v1::RuntimeEventType::CleanupCompleted,
                target,
                &report.reason_code,
                "worker process tree was reaped and its sandbox cleanup completed",
            );
        }
    }
}
