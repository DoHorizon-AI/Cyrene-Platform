//! KernelServiceAdapter 事件分发、历史追溯与运行时日志沉淀。

use std::sync::atomic::Ordering;

use cy_kernel_api::{semantic, CleanupReport, RuntimeJournalEvent, RuntimeJournalRecord};
use cy_proto::core_v1;

use crate::{
    adapter::{KernelServiceAdapter, OPERATION_EVENT_HISTORY_CAPACITY},
    convert::{now_timestamp, now_unix_ms, runtime_event_kind},
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
        self.publish_semantic_event(
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

    pub(crate) fn semantic_event_source(&self) -> semantic::Identity {
        semantic::Identity {
            id: format!("kernel/{}", self.daemon.node_id),
            generation: self.daemon.node_epoch,
        }
    }

    pub(crate) fn publish_semantic_event(
        &self,
        subject: semantic::Identity,
        kind: impl Into<String>,
        schema_id: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) {
        let event = semantic::Event {
            sequence: self
                .next_semantic_event_sequence
                .fetch_add(1, Ordering::Relaxed),
            source: self.semantic_event_source(),
            subject,
            kind: kind.into(),
            observed_at_unix_ms: now_unix_ms(),
            schema_id: schema_id.into(),
            body: body.into(),
        };
        if event.validate().is_err() {
            return;
        }
        let mut history = self
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        if history.len() == OPERATION_EVENT_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(event);
    }

    pub(crate) fn semantic_events_after(
        &self,
        cursor: &semantic::EventCursor,
        limit: usize,
    ) -> semantic::EventPage {
        let source = self.semantic_event_source();
        let history = self
            .semantic_events
            .lock()
            .expect("semantic event history lock poisoned");
        let oldest = history.front().map_or(0, |event| event.sequence);
        let latest = history.back().map_or(0, |event| event.sequence);
        let status = cursor.status_against(&source, oldest);
        if status != semantic::ReplayStatus::Current {
            return semantic::EventPage {
                source,
                status,
                events: Vec::new(),
                oldest_available_sequence: oldest,
                latest_available_sequence: latest,
                next_sequence: cursor.sequence,
            };
        }
        let events = history
            .iter()
            .filter(|event| event.sequence > cursor.sequence)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let next_sequence = events
            .last()
            .map_or(cursor.sequence, |event| event.sequence);
        semantic::EventPage {
            source,
            status,
            events,
            oldest_available_sequence: oldest,
            latest_available_sequence: latest,
            next_sequence,
        }
    }

    pub(crate) fn record_runtime(
        &self,
        event: RuntimeJournalEvent,
        instance_name: Option<&str>,
        lease: Option<&core_v1::ResourceLeaseRef>,
        reason_code: &str,
    ) {
        let _ = self.runtime_journal.append(RuntimeJournalRecord {
            event,
            node_id: self.daemon.node_id.clone(),
            node_epoch: self.daemon.node_epoch,
            instance_name: instance_name.map(str::to_owned),
            lease_name: lease.map(|lease| lease.lease_name.clone()),
            fence_token: lease.map(|lease| lease.fence_token),
            reason_code: reason_code.to_string(),
        });
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
