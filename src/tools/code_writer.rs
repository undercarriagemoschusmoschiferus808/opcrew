use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::file_ops::FileOpsTool;
use super::traits::{Tool, ToolParams, ToolResult};
use crate::error::{AgentError, Result};

pub struct CodeWriterTool;

impl CodeWriterTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CodeWriterTool {
    fn name(&self) -> &str {
        "code_writer"
    }

    fn description(&self) -> &str {
        "Write and edit source code files (local only, path denylist enforced)"
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
                tool: "code_writer".into(),
                message: "Missing 'path' argument".into(),
            })?;

        // Denylist check (inherits from FileOpsTool)
        if !FileOpsTool::is_path_allowed(path) {
            return Err(AgentError::PathDenied { path: path.clone() });
        }

        let result = tokio::time::timeout(timeout, async {
            match action {
                "create" => {
                    let content = params.args.get("content").cloned().unwrap_or_default();
                    if let Some(parent) = Path::new(path).parent() {
                        tokio::fs::create_dir_all(parent).await.ok();
                    }
                    tokio::fs::write(path, &content).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "code_writer".into(),
                            message: format!("Create failed: {e}"),
                        }
                    })?;
                    Ok(ToolResult::ok(
                        format!("Created {path} ({} bytes)", content.len()),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                "edit" => {
                    let old_text = params.args.get("old_text").ok_or_else(|| {
                        AgentError::ToolExecutionError {
                            tool: "code_writer".into(),
                            message: "Missing 'old_text' for edit".into(),
                        }
                    })?;
                    let new_text = params.args.get("new_text").ok_or_else(|| {
                        AgentError::ToolExecutionError {
                            tool: "code_writer".into(),
                            message: "Missing 'new_text' for edit".into(),
                        }
                    })?;

                    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "code_writer".into(),
                            message: format!("Read failed: {e}"),
                        }
                    })?;

                    if !content.contains(old_text.as_str()) {
                        return Ok(ToolResult::err(
                            format!("old_text not found in {path}"),
                            start.elapsed().as_millis() as u64,
                        ));
                    }

                    let updated = content.replacen(old_text.as_str(), new_text, 1);
                    tokio::fs::write(path, &updated).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "code_writer".into(),
                            message: format!("Write failed: {e}"),
                        }
                    })?;
                    Ok(ToolResult::ok(
                        format!("Edited {path}"),
                        start.elapsed().as_millis() as u64,
                    ))
                }
                _ => Err(AgentError::ToolExecutionError {
                    tool: "code_writer".into(),
                    message: format!("Unknown action: {action}. Use 'create' or 'edit'."),
                }),
            }
        })
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: "code_writer".into(),
            timeout_secs: timeout.as_secs(),
        })??;

        Ok(result)
    }
}
