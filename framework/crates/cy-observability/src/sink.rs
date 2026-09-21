//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 sink.rs                                                         │
//! │  Module: cy_observability::sink                                     │
//! │  Role: Bounded rolling file sink with single rotation ownership.    │
//! │                                                                     │
//! │  模块职责：单一轮转所有权的有界滚动文件 Sink 与故障弹性保障。          │
//! └─────────────────────────────────────────────────────────────────────┘

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

/// Default file size budget: 50 MiB.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Default maximum history retention count: 5 files.
pub const DEFAULT_MAX_HISTORY_FILES: usize = 5;

/// Configuration for controlled rolling file persistence.
#[derive(Debug, Clone)]
pub struct RollingFileConfig {
    pub directory: PathBuf,
    pub file_prefix: String,
    pub max_file_bytes: u64,
    pub max_history_files: usize,
}

impl RollingFileConfig {
    pub fn new(
        directory: impl Into<PathBuf>,
        file_prefix: impl Into<String>,
        max_file_bytes: u64,
        max_history_files: usize,
    ) -> Self {
        Self {
            directory: directory.into(),
            file_prefix: file_prefix.into(),
            max_file_bytes: if max_file_bytes == 0 {
                DEFAULT_MAX_FILE_BYTES
            } else {
                max_file_bytes
            },
            max_history_files: if max_history_files == 0 {
                DEFAULT_MAX_HISTORY_FILES
            } else {
                max_history_files
            },
        }
    }
}

/// ════════════════════════════════════════════════════════════════════════
/// Bounded Rolling File Sink.
///
/// Ensures strict single rotation ownership, bounded file sizes, bounded
/// retention history, and non-fatal degradation on filesystem errors.
/// ════════════════════════════════════════════════════════════════════════
pub struct BoundedRollingFileSink {
    directory: PathBuf,
    file_prefix: String,
    max_file_bytes: u64,
    max_history_files: usize,
    current_file: Option<File>,
    current_bytes: u64,
    dropped_writes: AtomicU64,
}

impl BoundedRollingFileSink {
    pub fn new(config: RollingFileConfig) -> io::Result<Self> {
        fs::create_dir_all(&config.directory)?;

        let active_path = config.directory.join(format!("{}.log", config.file_prefix));
        let existing_size = if active_path.exists() {
            fs::metadata(&active_path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)?;

        Ok(Self {
            directory: config.directory,
            file_prefix: config.file_prefix,
            max_file_bytes: config.max_file_bytes,
            max_history_files: config.max_history_files,
            current_file: Some(file),
            current_bytes: existing_size,
            dropped_writes: AtomicU64::new(0),
        })
    }

    /// Path to the active log file.
    pub fn active_file_path(&self) -> PathBuf {
        self.directory.join(format!("{}.log", self.file_prefix))
    }

    /// Number of write errors tracked.
    pub fn dropped_writes_count(&self) -> u64 {
        self.dropped_writes.load(Ordering::Relaxed)
    }

    /// Returns the current active file size in bytes.
    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    /// Executes atomic rotation of log files under single-owner authority.
    pub fn rotate(&mut self) -> io::Result<()> {
        // Drop current open file before renaming on Windows/Linux
        self.current_file = None;

        let active_path = self.active_file_path();
        if active_path.exists() {
            // 1. Remove oldest file if at history capacity
            let oldest_path = self
                .directory
                .join(format!("{}.log.{}", self.file_prefix, self.max_history_files));
            if oldest_path.exists() {
                let _ = fs::remove_file(&oldest_path);
            }

            // 2. Shift existing rotated archives down: .4 -> .5, .3 -> .4, etc.
            if self.max_history_files > 1 {
                for i in (1..self.max_history_files).rev() {
                    let from = self
                        .directory
                        .join(format!("{}.log.{}", self.file_prefix, i));
                    let to = self
                        .directory
                        .join(format!("{}.log.{}", self.file_prefix, i + 1));
                    if from.exists() {
                        let _ = fs::rename(&from, &to);
                    }
                }
            }

            // 3. Move current active to .1
            if self.max_history_files > 0 {
                let first_archive = self.directory.join(format!("{}.log.1", self.file_prefix));
                let _ = fs::rename(&active_path, &first_archive);
            } else {
                let _ = fs::remove_file(&active_path);
            }
        }

        // 4. Open a fresh active log file
        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&active_path)?;

        self.current_file = Some(new_file);
        self.current_bytes = 0;
        Ok(())
    }
}

impl Write for BoundedRollingFileSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let needed_bytes = buf.len() as u64;

        // Rotate if exceeding the bounded byte budget
        if self.current_bytes > 0 && (self.current_bytes + needed_bytes > self.max_file_bytes) {
            if let Err(err) = self.rotate() {
                self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                return Err(err);
            }
        }

        match self.current_file.as_mut() {
            Some(file) => match file.write(buf) {
                Ok(written) => {
                    self.current_bytes += written as u64;
                    Ok(written)
                }
                Err(err) => {
                    self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                    Err(err)
                }
            },
            None => {
                // Re-open attempt if file was dropped
                match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.active_file_path())
                {
                    Ok(mut file) => {
                        let res = file.write(buf);
                        match res {
                            Ok(written) => self.current_bytes += written as u64,
                            Err(_) => {
                                self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        self.current_file = Some(file);
                        res
                    }
                    Err(err) => {
                        self.dropped_writes.fetch_add(1, Ordering::Relaxed);
                        Err(err)
                    }
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.current_file.as_mut() {
            let res = file.flush();
            if res.is_err() {
                self.dropped_writes.fetch_add(1, Ordering::Relaxed);
            }
            res
        } else {
            Ok(())
        }
    }
}
