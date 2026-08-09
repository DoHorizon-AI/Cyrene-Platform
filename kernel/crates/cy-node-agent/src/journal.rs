//! Node Agent Local Persistent & Buffered Journal.
//!
//! Provides ring-buffered log entries, disk persistence, sequence tracking,
//! filtering, and real-time streaming support for AgentService.StreamJournal.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use cy_proto::{JournalEntry, JournalStreamRequest};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

/// Persistent record wrapping a JournalEntry with an auto-incrementing sequence number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalEntryRecord {
    pub sequence_number: u64,
    pub entry: JournalEntryInternal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalEntryInternal {
    pub target_id: String,
    pub timestamp: i64,
    pub source: String,
    pub log_level: String,
    pub message: String,
    pub metadata: HashMap<String, String>,
}

impl From<JournalEntryInternal> for JournalEntry {
    fn from(val: JournalEntryInternal) -> Self {
        JournalEntry {
            target_id: val.target_id,
            timestamp: val.timestamp,
            source: val.source,
            log_level: val.log_level,
            message: val.message,
            metadata: val.metadata,
        }
    }
}

/// Local persistent and buffered journal for Node Agent.
pub struct AgentJournal {
    target_id: String,
    max_capacity: usize,
    ring_buffer: VecDeque<JournalEntryRecord>,
    next_sequence: u64,
    file_path: Option<PathBuf>,
    broadcast_tx: broadcast::Sender<JournalEntryRecord>,
}

impl std::fmt::Debug for AgentJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentJournal")
            .field("target_id", &self.target_id)
            .field("max_capacity", &self.max_capacity)
            .field("entries_count", &self.ring_buffer.len())
            .field("next_sequence", &self.next_sequence)
            .field("file_path", &self.file_path)
            .finish()
    }
}

impl AgentJournal {
    pub fn new(target_id: &str, max_capacity: usize, file_path: Option<PathBuf>) -> Self {
        let (broadcast_tx, _) = broadcast::channel(1024);
        let mut journal = Self {
            target_id: target_id.to_string(),
            max_capacity: max_capacity.max(1),
            ring_buffer: VecDeque::new(),
            next_sequence: 1,
            file_path,
            broadcast_tx,
        };

        if journal.file_path.is_some() {
            let _ = journal.load_from_disk();
        }

        journal
    }

    /// Append a log entry to ring buffer and disk file, returning sequence number.
    pub fn append(
        &mut self,
        source: &str,
        log_level: &str,
        message: &str,
        metadata: HashMap<String, String>,
    ) -> Result<u64, JournalError> {
        let seq = self.next_sequence;
        self.next_sequence += 1;

        let timestamp = chrono::Utc::now().timestamp();
        let internal = JournalEntryInternal {
            target_id: self.target_id.clone(),
            timestamp,
            source: source.to_string(),
            log_level: log_level.to_string(),
            message: message.to_string(),
            metadata,
        };

        let record = JournalEntryRecord {
            sequence_number: seq,
            entry: internal,
        };

        // Ring buffer management
        if self.ring_buffer.len() >= self.max_capacity {
            self.ring_buffer.pop_front();
        }
        self.ring_buffer.push_back(record.clone());

        // Disk persistence
        if let Some(ref path) = self.file_path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            let json_line = serde_json::to_string(&record)?;
            writeln!(file, "{}", json_line)?;
        }

        // Broadcast to live stream subscribers
        let _ = self.broadcast_tx.send(record);

        Ok(seq)
    }

    /// Query historical log entries matching request filters.
    pub fn query(&self, req: &JournalStreamRequest) -> Vec<JournalEntryRecord> {
        let mut filtered: Vec<JournalEntryRecord> = self
            .ring_buffer
            .iter()
            .filter(|r| {
                if req.since_timestamp > 0 && r.entry.timestamp < req.since_timestamp {
                    return false;
                }
                if !req.filter_unit.is_empty() && r.entry.source != req.filter_unit {
                    return false;
                }
                true
            })
            .cloned()
            .collect();

        if req.tail_lines > 0 && filtered.len() > (req.tail_lines as usize) {
            let start = filtered.len() - (req.tail_lines as usize);
            filtered = filtered.split_off(start);
        }

        filtered
    }

    /// Subscribe to live stream updates.
    pub fn subscribe(&self) -> broadcast::Receiver<JournalEntryRecord> {
        self.broadcast_tx.subscribe()
    }

    /// Load persisted records from disk file.
    pub fn load_from_disk(&mut self) -> Result<(), JournalError> {
        let path = match self.file_path {
            Some(ref p) if p.exists() => p,
            _ => return Ok(()),
        };

        let file = File::open(path)?;
        let reader = BufReader::new(file);

        self.ring_buffer.clear();
        let mut max_seq = 0;

        for line in reader.lines() {
            let line_str = line?;
            if line_str.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<JournalEntryRecord>(&line_str) {
                if record.sequence_number > max_seq {
                    max_seq = record.sequence_number;
                }
                if self.ring_buffer.len() >= self.max_capacity {
                    self.ring_buffer.pop_front();
                }
                self.ring_buffer.push_back(record);
            }
        }

        if max_seq >= self.next_sequence {
            self.next_sequence = max_seq + 1;
        }

        Ok(())
    }

    pub fn len(&self) -> usize {
        self.ring_buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring_buffer.is_empty()
    }
}

pub type SharedAgentJournal = Arc<RwLock<AgentJournal>>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_journal_ring_buffer_and_query() {
        let mut journal = AgentJournal::new("target-1", 5, None);

        for i in 1..=10 {
            journal
                .append(
                    "cy-engine",
                    "INFO",
                    &format!("Log line {}", i),
                    HashMap::new(),
                )
                .unwrap();
        }

        // Capacity is 5, so only lines 6..=10 remain
        assert_eq!(journal.len(), 5);

        let req = JournalStreamRequest {
            target_id: "target-1".into(),
            follow: false,
            tail_lines: 3,
            filter_unit: "cy-engine".into(),
            since_timestamp: 0,
        };

        let queried = journal.query(&req);
        assert_eq!(queried.len(), 3);
        assert_eq!(queried[0].entry.message, "Log line 8");
        assert_eq!(queried[2].entry.message, "Log line 10");
        assert_eq!(queried[2].sequence_number, 10);
    }

    #[test]
    fn test_journal_disk_persistence() {
        let tmp_file = NamedTempFile::new().unwrap();
        let path = tmp_file.path().to_path_buf();

        {
            let mut journal = AgentJournal::new("target-disk", 100, Some(path.clone()));
            journal
                .append("sys", "WARN", "Disk warning 1", HashMap::new())
                .unwrap();
            journal
                .append("sys", "ERROR", "Disk error 2", HashMap::new())
                .unwrap();
        }

        // Reload from disk
        let mut reloaded = AgentJournal::new("target-disk", 100, Some(path));
        assert_eq!(reloaded.len(), 2);

        let req = JournalStreamRequest {
            target_id: "target-disk".into(),
            follow: false,
            tail_lines: 0,
            filter_unit: "".into(),
            since_timestamp: 0,
        };

        let entries = reloaded.query(&req);
        assert_eq!(entries[0].entry.message, "Disk warning 1");
        assert_eq!(entries[1].entry.message, "Disk error 2");
        assert_eq!(entries[1].sequence_number, 2);

        // Append again to check sequence continuation
        let seq = reloaded
            .append("sys", "INFO", "Disk info 3", HashMap::new())
            .unwrap();
        assert_eq!(seq, 3);
    }
}
