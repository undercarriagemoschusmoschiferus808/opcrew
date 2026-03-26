use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;

use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, MessageRole};
use crate::error::{AgentError, Result};
use crate::infra::graph::{InfraGraph, Service};
use crate::tools::shell::ShellTool;
use crate::tools::target::TargetHost;
use crate::tools::traits::{Tool, ToolParams, ToolResult};

/// Blocked patterns that bypass security controls.
const BLOCKED_PATTERNS: &[&str] = &[
    "bash -c",
    "sh -c",
    "/bin/bash -c",
    "/bin/sh -c",
    "eval ",
    "exec sh",
    "exec bash",
];

/// Read-only actions that can be auto-approved.
const READ_ONLY_ACTIONS: &[&str] = &["logs", "status", "config", "env"];

pub struct ServiceTool {
    client: Arc<dyn LlmProvider>,
    graph: Arc<RwLock<Option<InfraGraph>>>,
    target: TargetHost,
}

impl ServiceTool {
    pub fn new(
        client: Arc<dyn LlmProvider>,
        graph: Arc<RwLock<Option<InfraGraph>>>,
        target: TargetHost,
    ) -> Self {
        Self {
            client,
            graph,
            target,
        }
    }

    /// Translate an intent to shell commands via LLM.
    async fn translate(
        &self,
        service: &Service,
        action: &str,
        args: &HashMap<String, String>,
    ) -> Result<Vec<String>> {
        let ctx = service.execution_context.to_prompt_string();
        let args_str = args
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");

        let prompt = format!(
            "Service: {}\nRuntime: {}\nAction: {}\nArgs: {}\n\n\
             Return the command(s) needed, one per line.\n\
             For edit_config, return up to 4 commands.\n\
             Each command MUST be a single atomic operation.",
            service.name, ctx, action, args_str,
        );

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _) = self
            .client
            .send_message(TRANSLATION_PROMPT, &messages)
            .await?;

        // Parse response: one command per line
        let commands: Vec<String> = response
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("ERROR"))
            .collect();

        // Check for ERROR response
        if response.trim().starts_with("ERROR") {
            return Err(AgentError::ToolExecutionError {
                tool: "service".into(),
                message: response.trim().to_string(),
            });
        }

        if commands.is_empty() {
            return Err(AgentError::ToolExecutionError {
                tool: "service".into(),
                message: "LLM returned no commands for this action".into(),
            });
        }

        // Validate: reject composition and bash -c bypass
        for cmd in &commands {
            if ShellTool::has_composition(cmd) {
                return Err(AgentError::ShellComposition {
                    command: cmd.clone(),
                });
            }
            let lower = cmd.to_lowercase();
            for pattern in BLOCKED_PATTERNS {
                if lower.contains(pattern) {
                    return Err(AgentError::ToolExecutionError {
                        tool: "service".into(),
                        message: format!(
                            "Blocked: command contains '{pattern}' which bypasses security controls"
                        ),
                    });
                }
            }
        }

        Ok(commands)
    }

    /// Look up a service in the infra graph.
    fn find_service(&self, name: &str) -> Option<Service> {
        let graph = self.graph.read().unwrap();
        graph.as_ref()?.services.get(name).cloned()
    }

    pub fn is_read_only(action: &str) -> bool {
        READ_ONLY_ACTIONS.contains(&action)
    }
}

#[async_trait]
impl Tool for ServiceTool {
    fn name(&self) -> &str {
        "service"
    }

    fn description(&self) -> &str {
        "Execute actions on services (logs, status, restart, edit_config) — auto-translates to the correct runtime (Docker, K8s, systemd, etc.)"
    }

    fn is_remote_capable(&self) -> bool {
        true
    }

    async fn execute(&self, params: &ToolParams, timeout: Duration) -> Result<ToolResult> {
        let start = std::time::Instant::now();
        let service_name = params.args.get("service").ok_or_else(|| AgentError::ToolExecutionError {
            tool: "service".into(),
            message: "Missing 'service' argument. Use {\"tool\": \"service\", \"action\": \"logs\", \"args\": {\"service\": \"nginx\"}}".into(),
        })?;

        let action = &params.action;

        // Look up service in infra graph
        let service = self.find_service(service_name).ok_or_else(|| AgentError::ToolExecutionError {
            tool: "service".into(),
            message: format!("Service '{service_name}' not found in infra graph. Run `opcrew infra discover` first, or use 'shell' tool for direct commands."),
        })?;

        tracing::info!(
            service = %service_name,
            action = %action,
            runtime = %service.execution_context.runtime,
            "ServiceTool: translating intent"
        );

        // Translate intent to commands via LLM
        let commands = self.translate(&service, action, &params.args).await?;

        // Execute each command sequentially
        let shell = ShellTool::new(self.target.clone());
        let mut outputs = Vec::new();

        for (i, cmd) in commands.iter().enumerate() {
            tracing::info!(step = i + 1, total = commands.len(), cmd = %cmd, "ServiceTool: executing");
            eprintln!(
                "  {} [{}] {} → {}",
                colored::Colorize::cyan("⟐"),
                colored::Colorize::dimmed(service_name.as_str()),
                action,
                colored::Colorize::dimmed(cmd.as_str())
            );

            let shell_params = ToolParams {
                tool_name: "shell".into(),
                action: "run".into(),
                args: [("command".into(), cmd.clone())].into(),
            };

            match shell.execute(&shell_params, timeout).await {
                Ok(result) => {
                    if result.success {
                        outputs.push(format!("Step {}: {}\n{}", i + 1, cmd, result.output));
                    } else {
                        let error = result.error.unwrap_or_default();
                        outputs.push(format!("Step {} FAILED: {}\n{}", i + 1, cmd, error));
                        // Stop sequence on failure
                        return Ok(ToolResult::err(
                            format!(
                                "Step {}/{} failed: {cmd}\n{error}\nCompleted steps:\n{}",
                                i + 1,
                                commands.len(),
                                outputs.join("\n")
                            ),
                            start.elapsed().as_millis() as u64,
                        ));
                    }
                }
                Err(e) => {
                    return Ok(ToolResult::err(
                        format!("Step {}/{} error: {cmd}\n{e}", i + 1, commands.len()),
                        start.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        Ok(ToolResult::ok(
            outputs.join("\n---\n"),
            start.elapsed().as_millis() as u64,
        ))
    }
}

const TRANSLATION_PROMPT: &str = r#"You translate service action intents into atomic shell commands.

Given a service name, its runtime context, and an action, return the exact shell command(s) needed.

SAFETY RULES — MANDATORY:
- No shell composition: no ;, |, &&, ||, backticks, $()
- No bash -c or sh -c wrapping (this bypasses security controls)
- Each line = one single atomic command
- If you cannot translate safely, return: ERROR: <reason>

For multi-step actions (edit_config), return up to 4 commands, one per line.
Return ONLY the commands, no explanations."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_patterns_detection() {
        let lower = "docker exec test-web bash -c 'echo hello'".to_lowercase();
        assert!(BLOCKED_PATTERNS.iter().any(|p| lower.contains(p)));
    }

    #[test]
    fn read_only_actions() {
        assert!(ServiceTool::is_read_only("logs"));
        assert!(ServiceTool::is_read_only("status"));
        assert!(ServiceTool::is_read_only("config"));
        assert!(!ServiceTool::is_read_only("restart"));
        assert!(!ServiceTool::is_read_only("edit_config"));
    }

    #[test]
    fn execution_context_prompt() {
        let ctx = crate::infra::graph::ExecutionContext::docker("test-web");
        assert!(ctx.to_prompt_string().contains("docker"));
        assert!(ctx.to_prompt_string().contains("test-web"));

        let ctx = crate::infra::graph::ExecutionContext::kubernetes("prod", "nginx-abc");
        let s = ctx.to_prompt_string();
        assert!(s.contains("kubernetes"));
        assert!(s.contains("nginx-abc"));
        assert!(s.contains("prod"));
    }
}
