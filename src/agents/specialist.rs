use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use colored::Colorize;

use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, MessageRole};
use crate::domain::agent::{AgentBehavior, AgentConfig, AgentOutput};
use crate::error::Result;
use crate::execution::budget::TokenBudget;
use crate::observability::metrics::Metrics;
use crate::safety::guardian::{GuardianAgent, ReviewDecision};
use crate::safety::secrets::SecretMasker;
use crate::tools::registry::ToolRegistry;
use crate::tools::traits::ToolParams;

/// A specialist agent that executes tasks autonomously using tools.
/// Each tool call goes through the Guardian before execution.
pub struct SpecialistAgent {
    config: AgentConfig,
    client: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    guardian: Arc<GuardianAgent>,
    budget: Arc<TokenBudget>,
    masker: Arc<SecretMasker>,
    metrics: Arc<Metrics>,
}

impl SpecialistAgent {
    pub fn new(
        config: AgentConfig,
        client: Arc<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        guardian: Arc<GuardianAgent>,
        budget: Arc<TokenBudget>,
        masker: Arc<SecretMasker>,
        metrics: Arc<Metrics>,
    ) -> Self {
        budget.register_agent(&config.id.to_string());
        Self {
            config,
            client,
            tools,
            guardian,
            budget,
            masker,
            metrics,
        }
    }

    /// Run the autonomous agent loop: think → tool call → execute → observe → repeat.
    async fn run_loop(&self, task_input: &str) -> Result<AgentOutput> {
        let agent_id = self.config.id.to_string();
        let mut conversation: Vec<ChatMessage> = vec![ChatMessage {
            role: MessageRole::User,
            content: task_input.to_string(),
        }];

        let mut total_tokens: u32 = 0;
        let mut turn = 0;

        loop {
            turn += 1;

            // Check conversation cap
            if turn > self.config.max_conversation_turns {
                tracing::warn!(agent = %self.config.role, "Conversation cap reached, wrapping up");
                break;
            }

            // Check budget (85% work budget)
            let approaching = self.budget.agent_approaching_limit(&agent_id);
            if approaching {
                tracing::warn!(agent = %self.config.role, "Token budget approaching limit, wrapping up");
                // Use reserved budget for wrap-up
                conversation.push(ChatMessage {
                    role: MessageRole::User,
                    content: "You are running low on budget. Provide your final answer now with what you have so far.".into(),
                });
            }

            // Try to consume estimated tokens
            let estimated = 2000u32;
            if self
                .budget
                .try_consume(&agent_id, estimated, approaching)
                .is_err()
            {
                tracing::warn!(agent = %self.config.role, "Budget exceeded");
                break;
            }

            // Call Claude
            let (response, usage) = self
                .client
                .send_message(&self.config.system_prompt, &conversation)
                .await
                .inspect_err(|_| {
                    self.budget.adjust_actual(&agent_id, estimated, 0);
                })?;

            let actual_tokens = usage.input_tokens + usage.output_tokens;
            self.budget
                .adjust_actual(&agent_id, estimated, actual_tokens);
            total_tokens += actual_tokens;

            // Mask secrets in response before adding to conversation
            let masked_response = self.masker.mask_string(&response);
            conversation.push(ChatMessage {
                role: MessageRole::Assistant,
                content: masked_response.clone(),
            });

            // Check if the response contains a tool call (simple JSON detection)
            tracing::debug!(agent = %self.config.role, response_len = response.len(),
                response_preview = %&response[..response.len().min(300)], "Agent response");
            if let Some(tool_call) = extract_tool_call(&response) {
                // Real-time progress: show what's being executed
                let cmd_preview = tool_call
                    .args
                    .get("command")
                    .or(tool_call.args.get("path"))
                    .cloned()
                    .unwrap_or_else(|| tool_call.action.clone());
                let preview = &cmd_preview[..cmd_preview.len().min(80)];
                eprintln!(
                    "  {} [{}] {} {}",
                    "→".dimmed(),
                    self.config.role.cyan(),
                    tool_call.tool_name,
                    preview.dimmed()
                );
                tracing::info!(agent = %self.config.role, tool = %tool_call.tool_name,
                    action = %tool_call.action, "Tool call detected");
                // Guardian review
                let context = &conversation
                    .last()
                    .map(|m| m.content.clone())
                    .unwrap_or_default();

                let decision = self
                    .guardian
                    .review(&tool_call, &self.config.role, &agent_id, context)
                    .await?;

                match decision {
                    ReviewDecision::Approved { .. } => {
                        self.metrics.record_guardian_approval();
                        // Execute the tool
                        match self.tools.get(&tool_call.tool_name) {
                            Ok(tool) => {
                                let result =
                                    tool.execute(&tool_call, Duration::from_secs(60)).await;
                                let result_text = match result {
                                    Ok(r) => {
                                        if r.success {
                                            format!(
                                                "Tool output:\n{}",
                                                self.masker.mask_string(&r.output)
                                            )
                                        } else {
                                            format!("Tool error:\n{}", r.error.unwrap_or_default())
                                        }
                                    }
                                    Err(e) => format!("Tool execution failed: {e}"),
                                };
                                conversation.push(ChatMessage {
                                    role: MessageRole::User,
                                    content: result_text,
                                });
                            }
                            Err(e) => {
                                // Tool not found — tell agent to self-correct
                                conversation.push(ChatMessage {
                                    role: MessageRole::User,
                                    content: format!("{e}\nPlease choose a valid tool."),
                                });
                            }
                        }
                    }
                    ReviewDecision::Blocked { reason } => {
                        self.metrics.record_guardian_block();
                        conversation.push(ChatMessage {
                            role: MessageRole::User,
                            content: format!(
                                "Your tool call was BLOCKED by the Guardian: {reason}\n\
                                 Try an alternative approach."
                            ),
                        });
                    }
                    ReviewDecision::NeedsUserApproval { reason, .. } => {
                        self.metrics.record_guardian_prompt();
                        conversation.push(ChatMessage {
                            role: MessageRole::User,
                            content: format!(
                                "Your tool call was DENIED by the user: {reason}\n\
                                 Try an alternative approach."
                            ),
                        });
                    }
                }
            } else {
                // No tool call — this is the agent's final answer
                tracing::debug!(agent = %self.config.role, "No tool call detected, treating as final answer");
                break;
            }

            // Sliding window: if conversation is too long, summarize oldest turns
            if conversation.len() > 20 {
                let summary = summarize_conversation(&conversation[..10]);
                let remaining: Vec<ChatMessage> = conversation[10..].to_vec();
                conversation = vec![ChatMessage {
                    role: MessageRole::User,
                    content: format!("Previous context summary:\n{summary}\n\n{task_input}"),
                }];
                conversation.extend(remaining);
            }
        }

        // Extract final answer from last assistant message
        let final_content = conversation
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "No response generated".into());

        Ok(AgentOutput::new(
            self.config.id.clone(),
            self.config.role.clone(),
            final_content,
        )
        .with_tokens(total_tokens)
        .with_confidence(0.7))
    }
}

#[async_trait]
impl AgentBehavior for SpecialistAgent {
    fn config(&self) -> &AgentConfig {
        &self.config
    }

    async fn execute(&self, input: &str) -> Result<AgentOutput> {
        self.run_loop(input).await
    }
}

/// Extract a tool call from the agent's response.
/// Expects JSON like: {"tool": "shell", "action": "run", "args": {"command": "ls"}}
fn extract_tool_call(response: &str) -> Option<ToolParams> {
    // Try to find JSON in the response
    let json_str = extract_json_block(response)?;

    let value: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let obj = value.as_object()?;

    let tool_name = obj.get("tool")?.as_str()?.to_string();
    let action = obj.get("action")?.as_str()?.to_string();
    let args_val = obj.get("args")?;

    let args: HashMap<String, String> = if let Some(map) = args_val.as_object() {
        map.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    } else {
        HashMap::new()
    };

    Some(ToolParams {
        tool_name,
        action,
        args,
    })
}

fn extract_json_block(text: &str) -> Option<String> {
    // Find JSON object in the text
    let start = text.find('{')?;
    let mut depth = 0;
    let mut end = start;

    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    if depth == 0 && end > start {
        Some(text[start..end].to_string())
    } else {
        None
    }
}

fn summarize_conversation(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            };
            let truncated = if m.content.len() > 200 {
                format!("{}...", &m.content[..200])
            } else {
                m.content.clone()
            };
            format!("[{role}]: {truncated}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tool_call_from_response() {
        let response = r#"I need to check the logs.
{"tool": "shell", "action": "run", "args": {"command": "cat /var/log/nginx/error.log"}}
Let me analyze the output."#;

        let call = extract_tool_call(response).unwrap();
        assert_eq!(call.tool_name, "shell");
        assert_eq!(call.action, "run");
        assert_eq!(
            call.args.get("command").unwrap(),
            "cat /var/log/nginx/error.log"
        );
    }

    #[test]
    fn no_tool_call_returns_none() {
        let response = "The issue is likely a misconfigured upstream server.";
        assert!(extract_tool_call(response).is_none());
    }

    #[test]
    fn extract_json_block_nested() {
        let text = r#"Some text {"a": {"b": 1}} more text"#;
        let json = extract_json_block(text).unwrap();
        assert_eq!(json, r#"{"a": {"b": 1}}"#);
    }
}
