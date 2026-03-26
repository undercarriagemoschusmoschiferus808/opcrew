use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::process::Command;

use super::target::TargetHost;
use super::traits::{Tool, ToolParams, ToolResult};
use crate::error::{AgentError, Result};

/// Characters/operators that indicate shell composition — blocked for safety.
const COMPOSITION_CHARS: &[&str] = &[";", "&&", "||", "|", "`", "$(", ">>"];

pub struct ShellTool {
    target: TargetHost,
}

impl ShellTool {
    pub fn new(target: TargetHost) -> Self {
        Self { target }
    }

    /// Check if a command string contains unquoted shell composition operators.
    /// Walks the string character by character, tracking quote state.
    pub fn has_composition(command: &str) -> bool {
        let mut in_single_quote = false;
        let mut in_double_quote = false;
        let mut escaped = false;
        let chars: Vec<char> = command.chars().collect();
        let len = chars.len();

        for i in 0..len {
            let c = chars[i];

            if escaped {
                escaped = false;
                continue;
            }

            if c == '\\' && !in_single_quote {
                escaped = true;
                continue;
            }

            if c == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
                continue;
            }

            if c == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
                continue;
            }

            // Only check for operators outside quotes
            if in_single_quote || in_double_quote {
                continue;
            }

            // Check multi-char operators first
            if i + 1 < len {
                let two = &command[i..i + 2];
                if two == "&&" || two == "||" || two == "$(" {
                    return true;
                }
            }

            // Single-char operators
            if c == ';' || c == '|' || c == '`' {
                return true;
            }
        }

        false
    }

    fn parse_command(command: &str) -> Result<(String, Vec<String>)> {
        let tokens = shlex::split(command).ok_or_else(|| AgentError::ShellComposition {
            command: command.to_string(),
        })?;

        if tokens.is_empty() {
            return Err(AgentError::ToolExecutionError {
                tool: "shell".into(),
                message: "Empty command".into(),
            });
        }

        let program = tokens[0].clone();
        let args = tokens[1..].to_vec();
        Ok((program, args))
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute system commands with safety checks"
    }

    fn is_remote_capable(&self) -> bool {
        true
    }

    async fn execute(&self, params: &ToolParams, timeout: Duration) -> Result<ToolResult> {
        let command = params
            .args
            .get("command")
            .ok_or_else(|| AgentError::ToolExecutionError {
                tool: "shell".into(),
                message: "Missing 'command' argument".into(),
            })?;

        // Block shell composition
        if Self::has_composition(command) {
            return Err(AgentError::ShellComposition {
                command: command.clone(),
            });
        }

        let (program, args) = Self::parse_command(command)?;
        let start = Instant::now();

        let mut cmd = match &self.target {
            TargetHost::Local => {
                let mut c = Command::new(&program);
                c.args(&args);
                if let Some(cwd) = params.args.get("cwd") {
                    c.current_dir(cwd);
                }
                c
            }
            TargetHost::Remote { .. } => {
                let ssh_args = self.target.ssh_args();
                let mut c = Command::new(&ssh_args[0]);
                c.args(&ssh_args[1..]);
                c.arg(command); // Pass full command to SSH
                c
            }
        };

        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| AgentError::ToolExecutionError {
                tool: "shell".into(),
                message: format!("Failed to spawn: {e}"),
            })?;

        // Per-call timeout — use select! so we keep ownership of child for kill
        let result = tokio::time::timeout(timeout, async {
            let status = child
                .wait()
                .await
                .map_err(|e| AgentError::ToolExecutionError {
                    tool: "shell".into(),
                    message: format!("Wait failed: {e}"),
                })?;

            let stdout = if let Some(mut out) = child.stdout.take() {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf)
                    .await
                    .ok();
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            let stderr = if let Some(mut err) = child.stderr.take() {
                let mut buf = Vec::new();
                tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf)
                    .await
                    .ok();
                String::from_utf8_lossy(&buf).to_string()
            } else {
                String::new()
            };

            Ok::<_, AgentError>((status, stdout, stderr))
        })
        .await;

        match result {
            Ok(Ok((status, stdout, stderr))) => {
                let duration = start.elapsed().as_millis() as u64;
                if status.success() {
                    Ok(ToolResult::ok(stdout, duration))
                } else {
                    Ok(ToolResult::err(
                        format!(
                            "Exit code: {}\nstdout: {}\nstderr: {}",
                            status.code().unwrap_or(-1),
                            stdout,
                            stderr
                        ),
                        duration,
                    ))
                }
            }
            Ok(Err(e)) => {
                let duration = start.elapsed().as_millis() as u64;
                Ok(ToolResult::err(format!("Error: {e}"), duration))
            }
            Err(_) => {
                // Timeout — kill explicitly (kill_on_drop is backup)
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(AgentError::ToolTimeout {
                    tool: "shell".into(),
                    timeout_secs: timeout.as_secs(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn detects_semicolon_composition() {
        assert!(ShellTool::has_composition("ls; rm -rf /"));
    }

    #[test]
    fn detects_pipe_composition() {
        assert!(ShellTool::has_composition("cat file | grep secret"));
    }

    #[test]
    fn detects_and_composition() {
        assert!(ShellTool::has_composition("ls && rm -rf /"));
    }

    #[test]
    fn allows_quoted_semicolon() {
        // "hello; world" inside quotes is safe
        assert!(!ShellTool::has_composition(r#"echo "hello; world""#));
    }

    #[test]
    fn allows_simple_commands() {
        assert!(!ShellTool::has_composition("ls -la /tmp"));
        assert!(!ShellTool::has_composition("cat /var/log/syslog"));
        assert!(!ShellTool::has_composition("ps aux"));
    }

    #[test]
    fn parse_command_basic() {
        let (prog, args) = ShellTool::parse_command("ls -la /tmp").unwrap();
        assert_eq!(prog, "ls");
        assert_eq!(args, vec!["-la", "/tmp"]);
    }

    #[tokio::test]
    async fn execute_echo() {
        let tool = ShellTool::new(TargetHost::Local);
        let params = ToolParams {
            tool_name: "shell".into(),
            action: "run".into(),
            args: HashMap::from([("command".into(), "echo hello".into())]),
        };
        let result = tool
            .execute(&params, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output.trim(), "hello");
    }

    #[tokio::test]
    async fn rejects_composition() {
        let tool = ShellTool::new(TargetHost::Local);
        let params = ToolParams {
            tool_name: "shell".into(),
            action: "run".into(),
            args: HashMap::from([("command".into(), "ls; rm -rf /".into())]),
        };
        let result = tool.execute(&params, Duration::from_secs(10)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        let tool = ShellTool::new(TargetHost::Local);
        let params = ToolParams {
            tool_name: "shell".into(),
            action: "run".into(),
            args: HashMap::from([("command".into(), "sleep 60".into())]),
        };
        let result = tool.execute(&params, Duration::from_millis(100)).await;
        assert!(matches!(result, Err(AgentError::ToolTimeout { .. })));
    }
}
