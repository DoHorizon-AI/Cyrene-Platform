// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: agents/node/cy-node-agent/src/upgrade.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Agent Self-Upgrade handler.
//!
//! Handles checksum verification, update payload staging, atomic file replacement,
//! and restart sequence execution on the node agent.
//! 负责校验 checksum、暂存更新 payload、原子替换文件，并在 Node Agent 上执行重启流程。

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UpgradeError {
    #[error("Checksum verification failed: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("Invalid upgrade payload: {0}")]
    InvalidPayload(String),

    #[error("Staging binary failed: {0}")]
    StagingFailed(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Agent Self-Upgrader.
/// Agent 自升级器。
#[derive(Debug, Clone)]
pub struct AgentUpgrader {
    current_version: String,
    agent_dir: PathBuf,
}

impl AgentUpgrader {
    pub fn new(current_version: impl Into<String>, agent_dir: impl AsRef<Path>) -> Self {
        Self {
            current_version: current_version.into(),
            agent_dir: agent_dir.as_ref().to_path_buf(),
        }
    }

    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    /// Calculate SHA256 checksum of data slice.
    /// 计算数据切片的 SHA-256 checksum。
    pub fn calculate_sha256(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// Verify payload, stage binary to disk, and trigger restart sequence.
    /// 校验 payload，将 binary 暂存到磁盘，并触发重启流程。
    pub fn apply_upgrade(
        &mut self,
        target_version: &str,
        binary_bytes: &[u8],
        expected_sha256: &str,
    ) -> Result<String, UpgradeError> {
        if binary_bytes.is_empty() {
            return Err(UpgradeError::InvalidPayload(
                "Binary payload is empty".into(),
            ));
        }

        // 1. Verify SHA-256 binary hash
        // 1. 校验 binary 的 SHA-256 hash
        let actual_hash = Self::calculate_sha256(binary_bytes);
        if actual_hash.to_lowercase() != expected_sha256.to_lowercase() {
            return Err(UpgradeError::ChecksumMismatch {
                expected: expected_sha256.to_string(),
                actual: actual_hash,
            });
        }

        // 2. Stage binary payload to staging file
        // 2. 将 binary payload 写入暂存文件
        std::fs::create_dir_all(&self.agent_dir)?;
        let temp_bin_path = self.agent_dir.join("cy-node-agent.tmp");
        let final_bin_path = self.agent_dir.join("cy-node-agent");

        std::fs::write(&temp_bin_path, binary_bytes)?;

        // Set executable permissions on unix-like systems
        // 在类 Unix 系统上设置可执行权限
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&temp_bin_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&temp_bin_path, perms)?;
        }

        // 3. Atomic rename/replace
        // 3. 原子重命名/替换
        std::fs::rename(&temp_bin_path, &final_bin_path)?;

        let old_ver = self.current_version.clone();
        self.current_version = target_version.to_string();

        Ok(format!(
            "Successfully upgraded cy-node-agent from v{} to v{}. Staged binary at {}",
            old_ver,
            target_version,
            final_bin_path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_self_upgrade_success() {
        let temp_dir = TempDir::new().unwrap();
        let mut upgrader = AgentUpgrader::new("0.1.0", temp_dir.path());

        let binary_payload = b"new agent binary v0.2.0 content".to_vec();
        let sha256 = AgentUpgrader::calculate_sha256(&binary_payload);

        let res = upgrader.apply_upgrade("0.2.0", &binary_payload, &sha256);
        assert!(res.is_ok());
        assert_eq!(upgrader.current_version(), "0.2.0");

        let staged_binary = temp_dir.path().join("cy-node-agent");
        assert!(staged_binary.exists());
        assert_eq!(std::fs::read(staged_binary).unwrap(), binary_payload);
    }

    #[test]
    fn test_self_upgrade_hash_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let mut upgrader = AgentUpgrader::new("0.1.0", temp_dir.path());

        let binary_payload = b"binary payload".to_vec();
        let invalid_hash = "0000000000000000000000000000000000000000000000000000000000000000";

        let err = upgrader
            .apply_upgrade("0.2.0", &binary_payload, invalid_hash)
            .unwrap_err();
        assert!(matches!(err, UpgradeError::ChecksumMismatch { .. }));
        assert_eq!(upgrader.current_version(), "0.1.0");
    }
}
