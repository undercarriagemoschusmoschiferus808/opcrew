use crate::error::Result;
use crate::safety::audit::AuditLog;

/// Export interface for audit logs.
pub enum ExportTarget {
    File,   // Default — already written by AuditLog
    Stdout, // Print to stdout for piping
}

pub fn export_audit_log(audit_log: &AuditLog, target: ExportTarget) -> Result<()> {
    match target {
        ExportTarget::File => {
            // Already handled by AuditLog::log()
            Ok(())
        }
        ExportTarget::Stdout => {
            let entries = audit_log.read_entries()?;
            for entry in entries {
                if let Ok(json) = serde_json::to_string(&entry) {
                    println!("{json}");
                }
            }
            Ok(())
        }
    }
}
