// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-kernel-contract/src/event.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 语义事件、游标与分页回放模型。

use crate::{
    identity::Identity,
    validation::{
        validate_namespaced_id, validate_timestamp, ContractError, MAX_EVENTS_PER_PAGE,
        MAX_EVENT_BODY_BYTES,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub sequence: u64,
    pub source: Identity,
    pub subject: Identity,
    pub kind: String,
    pub observed_at_unix_ms: u64,
    pub schema_id: String,
    pub body: Vec<u8>,
}

impl Event {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.sequence == 0 {
            return Err(ContractError::new(
                "EVENT_SEQUENCE_INVALID",
                "event sequence must be non-zero",
            ));
        }
        self.source.validate()?;
        self.subject.validate()?;
        validate_namespaced_id("event kind", &self.kind)?;
        validate_timestamp("event observed time", self.observed_at_unix_ms)?;
        validate_namespaced_id("event schema id", &self.schema_id)?;
        if self.body.len() > MAX_EVENT_BODY_BYTES {
            return Err(ContractError::new(
                "EVENT_BODY_LIMIT_EXCEEDED",
                "event body exceeds the semantic contract limit",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCursor {
    pub source: Identity,
    pub sequence: u64,
}

impl EventCursor {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.source.validate()
    }

    pub fn status_against(
        &self,
        current_source: &Identity,
        oldest_available_sequence: u64,
    ) -> ReplayStatus {
        if &self.source != current_source {
            ReplayStatus::SourceChanged
        } else if oldest_available_sequence > 0
            && self.sequence.saturating_add(1) < oldest_available_sequence
        {
            ReplayStatus::Gap
        } else {
            ReplayStatus::Current
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayStatus {
    Current,
    Gap,
    SourceChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub source: Identity,
    pub status: ReplayStatus,
    pub events: Vec<Event>,
    pub oldest_available_sequence: u64,
    pub latest_available_sequence: u64,
    pub next_sequence: u64,
}

impl EventPage {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.source.validate()?;
        if self.events.len() > MAX_EVENTS_PER_PAGE {
            return Err(ContractError::new(
                "EVENT_PAGE_LIMIT_EXCEEDED",
                "event page exceeds the semantic contract limit",
            ));
        }
        if self.oldest_available_sequence > self.latest_available_sequence {
            return Err(ContractError::new(
                "EVENT_RANGE_INVALID",
                "event replay range is inverted",
            ));
        }
        if (self.oldest_available_sequence == 0) != (self.latest_available_sequence == 0) {
            return Err(ContractError::new(
                "EVENT_RANGE_INVALID",
                "event replay range must be wholly empty or wholly non-zero",
            ));
        }
        if self.status != ReplayStatus::Current && !self.events.is_empty() {
            return Err(ContractError::new(
                "EVENT_RECONCILE_REQUIRED",
                "gap and source-change pages cannot contain incremental events",
            ));
        }
        let mut previous = None;
        for event in &self.events {
            event.validate()?;
            if event.source != self.source
                || previous.is_some_and(|sequence| event.sequence <= sequence)
                || event.sequence < self.oldest_available_sequence
                || event.sequence > self.latest_available_sequence
            {
                return Err(ContractError::new(
                    "EVENT_ORDER_INVALID",
                    "event page sources must match and sequences must strictly increase",
                ));
            }
            previous = Some(event.sequence);
        }
        if let Some(last) = previous {
            if self.next_sequence != last {
                return Err(ContractError::new(
                    "EVENT_CURSOR_INVALID",
                    "event page cursor must equal the last returned sequence",
                ));
            }
        }
        Ok(())
    }
}
