use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::traits::{Tool, ToolParams, ToolResult};
use crate::error::{AgentError, Result};

/// Hardcoded denylist — independent of Guardian. Defense in depth.
const DENIED_PATHS: &[&str] = &[
    "/etc/",
    "/boot/",
    "/sys/",
    "/proc/",
    "/dev/",
    "/sbin/",
    "/usr/sbin/",
    "/root/",
];

pub struct FileOpsTool {
    allowed_write_roots: Option<Vec<String>>,
}

impl FileOpsTool {
    pub fn new(allowed_write_roots: Option<Vec<String>>) -> Self {
        Self {
            allowed_write_roots,
        }
    }

    pub fn is_path_allowed(path: &str) -> bool {
        let normalized = if path.starts_with('/') {
            path.to_string()
        } else {
            // Resolve relative paths
            std::env::current_dir()
                .map(|cwd| cwd.join(path).to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string())
        };

        for denied in DENIED_PATHS {
            if normalized.starts_with(denied) {
                return false;
            }
        }
        true
    }

    fn check_write_roots(&self, path: &str) -> bool {
        if let Some(roots) = &self.allowed_write_roots {
            roots.iter().any(|root| path.starts_with(root))
        } else {
            true // No restriction
        }
    }
}

#[async_trait]
impl Tool for FileOpsTool {
    fn name(&self) -> &str {
        "file_ops"
    }

    fn description(&self) -> &str {
        "Create, read, write, delete files (local only, path denylist enforced)"
    }

    fn is_remote_capable(&self) -> bool {
        false // Local only
    }

    async fn execute(&self, params: &ToolParams, timeout: Duration) -> Result<ToolResult> {
        let start = Instant::now();

        let action = params.action.as_str();
        let path = params
            .args
            .get("path")
            .ok_or_else(|| AgentError::ToolExecutionError {
                tool: "file_ops".into(),
                message: "Missing 'path' argument".into(),
            })?;

        // Denylist check
        if !Self::is_path_allowed(path) {
            return Err(AgentError::PathDenied { path: path.clone() });
        }

        let result = tokio::time::timeout(timeout, async {
            match action {
                "read" => {
                    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "file_ops".into(),
                            message: format!("Read failed: {e}"),
                        }
                    })?;
                    Ok(ToolResult::ok(content, start.elapsed().as_millis() as u64))
                }
                "write" => {
                    if !self.check_write_roots(path) {
                        return Err(AgentError::PathDenied { path: path.clone() });
                    }
                    let content = params.args.get("content").cloned().unwrap_or_default();
                    // Create parent dirs if needed
                    if let Some(parent) = Path::new(path).parent() {
                        tokio::fs::create_dir_all(parent).await.ok();
                    }
                    tokio::fs::write(path, &content).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "file_ops".into(),
                            message: format!("Write failed: {e}"),
                        }
                    })?;
                    Ok(ToolResult::ok(
                        format!("Written {} bytes to {path}", content.len()),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                "list" => {
                    let mut entries = Vec::new();
                    let mut dir = tokio::fs::read_dir(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "file_ops".into(),
                            message: format!("List failed: {e}"),
                        }
                    })?;
                    while let Some(entry) =
                        dir.next_entry()
                            .await
                            .map_err(|e| AgentError::ToolExecutionError {
                                tool: "file_ops".into(),
                                message: format!("Read entry: {e}"),
                            })?
                    {
                        entries.push(entry.file_name().to_string_lossy().to_string());
                    }
                    entries.sort();
                    Ok(ToolResult::ok(
                        entries.join("\n"),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                "exists" => {
                    let exists = Path::new(path).exists();
                    Ok(ToolResult::ok(
                        exists.to_string(),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                "delete" => {
                    if !self.check_write_roots(path) {
                        return Err(AgentError::PathDenied { path: path.clone() });
                    }
                    tokio::fs::remove_file(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "file_ops".into(),
                            message: format!("Delete failed: {e}"),
                        }
                    })?;
                    Ok(ToolResult::ok(
                        format!("Deleted {path}"),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                _ => Err(AgentError::ToolExecutionError {
                    tool: "file_ops".into(),
                    message: format!("Unknown action: {action}"),
                }),
            }
        })
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: "file_ops".into(),
            timeout_secs: timeout.as_secs(),
        })??;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_system_paths() {
        assert!(!FileOpsTool::is_path_allowed("/etc/passwd"));
        assert!(!FileOpsTool::is_path_allowed("/boot/grub/grub.cfg"));
        assert!(!FileOpsTool::is_path_allowed("/proc/1/status"));
        assert!(!FileOpsTool::is_path_allowed("/sys/class/net"));
        assert!(!FileOpsTool::is_path_allowed("/root/.bashrc"));
    }

    #[test]
    fn allows_safe_paths() {
        assert!(FileOpsTool::is_path_allowed("/home/user/project/file.txt"));
        assert!(FileOpsTool::is_path_allowed("/tmp/test.log"));
        assert!(FileOpsTool::is_path_allowed("/var/log/nginx/error.log"));
    }

    #[tokio::test]
    async fn read_nonexistent_file() {
        let tool = FileOpsTool::new(None);
        let params = ToolParams {
            tool_name: "file_ops".into(),
            action: "read".into(),
            args: [("path".into(), "/tmp/nonexistent_test_file_12345".into())].into(),
        };
        let result = tool.execute(&params, Duration::from_secs(5)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_to_denied_path() {
        let tool = FileOpsTool::new(None);
        let params = ToolParams {
            tool_name: "file_ops".into(),
            action: "write".into(),
            args: [
                ("path".into(), "/etc/test".into()),
                ("content".into(), "test".into()),
            ]
            .into(),
        };
        let result = tool.execute(&params, Duration::from_secs(5)).await;
        assert!(matches!(result, Err(AgentError::PathDenied { .. })));
    }
}
