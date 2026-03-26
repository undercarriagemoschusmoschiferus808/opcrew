use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::safety::secrets::SecretMasker;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub session_id: Uuid,
    pub agent_id: Option<String>,
    pub agent_role: Option<String>,
    pub task_id: Option<String>,
    pub action: AuditAction,
    pub tool_name: Option<String>,
    pub params: Option<serde_json::Value>,
    pub risk_level: Option<String>,
    pub decision: Option<String>,
    pub result_success: Option<bool>,
    pub result_output: Option<String>,
    pub tokens_used: Option<u32>,
    pub hmac: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditAction {
    ToolRequest,
    ToolResult,
    GuardianDecision,
    UserDecision,
    PlanCreated,
    AgentStarted,
    AgentCompleted,
    SessionStarted,
    SessionCompleted,
}

pub struct AuditLog {
    path: PathBuf,
    session_id: Uuid,
    hmac_key: Vec<u8>,
    masker: SecretMasker,
    max_size_bytes: u64,
    max_rotations: u32,
}

impl AuditLog {
    pub fn new(path: PathBuf, session_id: Uuid, masker: SecretMasker, max_size_mb: u64) -> Self {
        // Derive HMAC key from session_id (not cryptographic proof, but tamper detection)
        let hmac_key = format!("audit-{session_id}").into_bytes();
        Self {
            path,
            session_id,
            hmac_key,
            masker,
            max_size_bytes: max_size_mb * 1024 * 1024,
            max_rotations: 5,
        }
    }

    pub fn log(&self, mut entry: AuditEntry) -> Result<()> {
        entry.session_id = self.session_id;

        // Mask secrets in params and output
        if let Some(params) = &entry.params {
            let masked = self.masker.mask_value(params);
            entry.params = Some(masked);
        }
        if let Some(output) = &entry.result_output {
            entry.result_output = Some(self.masker.mask_string(output));
        }

        // Compute HMAC
        let json_for_hmac = serde_json::to_string(&entry)
            .map_err(|e| AgentError::AuditError(format!("Serialize: {e}")))?;
        entry.hmac = Some(self.compute_hmac(&json_for_hmac));

        // Serialize final entry
        let line = serde_json::to_string(&entry)
            .map_err(|e| AgentError::AuditError(format!("Serialize: {e}")))?;

        // Check rotation
        self.rotate_if_needed()?;

        // Append with fsync
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| AgentError::AuditError(format!("Open: {e}")))?;

        writeln!(file, "{line}").map_err(|e| AgentError::AuditError(format!("Write: {e}")))?;
        file.sync_all()
            .map_err(|e| AgentError::AuditError(format!("Fsync: {e}")))?;

        Ok(())
    }

    pub fn create_entry(&self, action: AuditAction) -> AuditEntry {
        AuditEntry {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            session_id: self.session_id,
            agent_id: None,
            agent_role: None,
            task_id: None,
            action,
            tool_name: None,
            params: None,
            risk_level: None,
            decision: None,
            result_success: None,
            result_output: None,
            tokens_used: None,
            hmac: None,
        }
    }

    /// Read all entries from the audit log, skipping corrupted lines.
    pub fn read_entries(&self) -> Result<Vec<AuditEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&self.path)
            .map_err(|e| AgentError::AuditError(format!("Read: {e}")))?;

        let mut entries = Vec::new();
        for (i, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!(line_number = i + 1, error = %e, "Skipping corrupted audit entry");
                }
            }
        }
        Ok(entries)
    }

    /// Verify HMAC integrity of entries. Returns indices of tampered entries.
    pub fn verify_integrity(&self, entries: &[AuditEntry]) -> Vec<usize> {
        let mut tampered = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            if let Some(stored_hmac) = &entry.hmac {
                let mut check_entry = entry.clone();
                check_entry.hmac = None;
                if let Ok(json) = serde_json::to_string(&check_entry) {
                    let computed = self.compute_hmac(&json);
                    if &computed != stored_hmac {
                        tampered.push(i);
                    }
                }
            }
        }
        tampered
    }

    /// Find entries for this session that are completed.
    pub fn completed_task_ids(&self) -> Result<Vec<String>> {
        let entries = self.read_entries()?;
        Ok(entries
            .iter()
            .filter(|e| e.session_id == self.session_id)
            .filter(|e| matches!(e.action, AuditAction::AgentCompleted))
            .filter_map(|e| e.task_id.clone())
            .collect())
    }

    fn compute_hmac(&self, data: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.hmac_key).expect("HMAC accepts any key length");
        mac.update(data.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn rotate_if_needed(&self) -> Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let metadata = std::fs::metadata(&self.path)
            .map_err(|e| AgentError::AuditError(format!("Metadata: {e}")))?;

        if metadata.len() < self.max_size_bytes {
            return Ok(());
        }

        // Rotate: audit.log.4 → delete, audit.log.3 → .4, ..., audit.log → .1
        for i in (1..self.max_rotations).rev() {
            let from = self.rotated_path(i);
            let to = self.rotated_path(i + 1);
            if from.exists() {
                std::fs::rename(&from, &to).ok();
            }
        }
        let first_rotation = self.rotated_path(1);
        std::fs::rename(&self.path, &first_rotation)
            .map_err(|e| AgentError::AuditError(format!("Rotate: {e}")))?;

        Ok(())
    }

    fn rotated_path(&self, n: u32) -> PathBuf {
        let name = self.path.to_string_lossy();
        PathBuf::from(format!("{name}.{n}"))
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn masker(&self) -> &SecretMasker {
        &self.masker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn create_and_read_entries() {
        let dir = std::env::temp_dir().join(format!("audit_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");

        let log = AuditLog::new(path.clone(), Uuid::new_v4(), SecretMasker::new(), 50);

        let entry = log.create_entry(AuditAction::SessionStarted);
        log.log(entry).unwrap();

        let entries = log.read_entries().unwrap();
        assert_eq!(entries.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hmac_integrity_check() {
        let dir = std::env::temp_dir().join(format!("audit_hmac_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");

        let log = AuditLog::new(path.clone(), Uuid::new_v4(), SecretMasker::new(), 50);

        let entry = log.create_entry(AuditAction::SessionStarted);
        log.log(entry).unwrap();

        let entries = log.read_entries().unwrap();
        let tampered = log.verify_integrity(&entries);
        assert!(tampered.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_corrupted_lines() {
        let dir = std::env::temp_dir().join(format!("audit_corrupt_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.log");

        let log = AuditLog::new(path.clone(), Uuid::new_v4(), SecretMasker::new(), 50);
        let entry = log.create_entry(AuditAction::SessionStarted);
        log.log(entry).unwrap();

        // Append corrupted line
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{{corrupted json").unwrap();

        let entries = log.read_entries().unwrap();
        assert_eq!(entries.len(), 1); // Only the valid entry

        std::fs::remove_dir_all(&dir).ok();
    }
}
