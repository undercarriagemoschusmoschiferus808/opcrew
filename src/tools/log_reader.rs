use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::traits::{Tool, ToolParams, ToolResult};
use crate::error::{AgentError, Result};

pub struct LogReaderTool;

impl LogReaderTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for LogReaderTool {
    fn name(&self) -> &str {
        "log_reader"
    }

    fn description(&self) -> &str {
        "Read and search log files (local and remote via SSH)"
    }

    fn is_remote_capable(&self) -> bool {
        true
    }

    async fn execute(&self, params: &ToolParams, timeout: Duration) -> Result<ToolResult> {
        let start = Instant::now();
        let action = params.action.as_str();
        let path = params
            .args
            .get("path")
            .ok_or_else(|| AgentError::ToolExecutionError {
                tool: "log_reader".into(),
                message: "Missing 'path' argument".into(),
            })?;

        let result = tokio::time::timeout(timeout, async {
            match action {
                "read" => {
                    let lines: usize = params
                        .args
                        .get("lines")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(100);

                    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "log_reader".into(),
                            message: format!("Read failed: {e}"),
                        }
                    })?;

                    // Return last N lines (tail behavior)
                    let tail: String = content
                        .lines()
                        .rev()
                        .take(lines)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");

                    Ok(ToolResult::ok(tail, start.elapsed().as_millis() as u64))
                }
                "search" => {
                    let pattern = params.args.get("pattern").ok_or_else(|| {
                        AgentError::ToolExecutionError {
                            tool: "log_reader".into(),
                            message: "Missing 'pattern' for search".into(),
                        }
                    })?;

                    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                        AgentError::ToolExecutionError {
                            tool: "log_reader".into(),
                            message: format!("Read failed: {e}"),
                        }
                    })?;

                    let matches: Vec<&str> = content
                        .lines()
                        .filter(|line| line.contains(pattern.as_str()))
                        .collect();

                    let max_results: usize = params
                        .args
                        .get("max_results")
                        .and_then(|m| m.parse().ok())
                        .unwrap_or(50);

                    let output: String = matches
                        .into_iter()
                        .take(max_results)
                        .collect::<Vec<_>>()
                        .join("\n");

                    Ok(ToolResult::ok(output, start.elapsed().as_millis() as u64))
                }
                _ => Err(AgentError::ToolExecutionError {
                    tool: "log_reader".into(),
                    message: format!("Unknown action: {action}. Use 'read' or 'search'."),
                }),
            }
        })
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: "log_reader".into(),
            timeout_secs: timeout.as_secs(),
        })??;

        Ok(result)
    }
}
