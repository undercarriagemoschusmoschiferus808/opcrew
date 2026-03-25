use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::api::provider::LlmProvider;
use crate::api::schema::extract_json;
use crate::api::types::{ChatMessage, MessageRole};
use crate::domain::agent::{AgentBehavior, AgentConfig, AgentId, AgentOutput};
use crate::error::Result;
use crate::observability::metrics::Metrics;
use crate::safety::guardian::GuardianAgent;
use crate::tools::registry::ToolRegistry;
use crate::tools::traits::{Tool, ToolParams};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum VerificationResult {
    Resolved { confidence: f32, evidence: String },
    PartiallyResolved { confidence: f32, what_worked: String, what_remains: String },
    Failed { reason: String, suggested_next_steps: String },
    Inconclusive { reason: String },
    Regressed { original_fixed: String, new_issue: String, evidence: String },
}

impl VerificationResult {
    pub fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved { .. })
    }

    pub fn parse(response: &str) -> Self {
        let json_str = extract_json(response);
        if let Ok(result) = serde_json::from_str::<VerificationResult>(&json_str) {
            return result;
        }
        let upper = response.to_uppercase();
        if upper.contains("RESOLVED") && !upper.contains("PARTIALLY") && !upper.contains("REGRESS") {
            VerificationResult::Resolved { confidence: 0.7, evidence: response.to_string() }
        } else if upper.contains("REGRESS") {
            VerificationResult::Regressed {
                original_fixed: "Unknown".into(), new_issue: response.to_string(), evidence: response.to_string(),
            }
        } else if upper.contains("PARTIALLY") {
            VerificationResult::PartiallyResolved {
                confidence: 0.4, what_worked: "Unknown".into(), what_remains: response.to_string(),
            }
        } else if upper.contains("FAILED") || upper.contains("NOT RESOLVED") {
            VerificationResult::Failed { reason: response.to_string(), suggested_next_steps: String::new() }
        } else {
            VerificationResult::Inconclusive { reason: response.to_string() }
        }
    }
}

/// Verifier that actually executes read-only commands to check system state.
pub struct VerifierAgent {
    config: AgentConfig,
    client: Arc<dyn LlmProvider>,
    tools: Arc<ToolRegistry>,
    guardian: Arc<GuardianAgent>,
    metrics: Arc<Metrics>,
}

impl VerifierAgent {
    pub fn new(
        client: Arc<dyn LlmProvider>,
        tools: Arc<ToolRegistry>,
        guardian: Arc<GuardianAgent>,
        metrics: Arc<Metrics>,
    ) -> Self {
        let config = AgentConfig {
            id: AgentId::new(),
            role: "Verifier".to_string(),
            expertise: vec!["verification".into(), "regression testing".into()],
            system_prompt: VERIFIER_SYSTEM_PROMPT.to_string(),
            goal: "Verify if the problem is actually resolved".to_string(),
            allowed_tools: vec!["shell".into(), "log_reader".into()],
            token_budget: 200_000,
            max_conversation_turns: 15,
        };
        Self { config, client, tools, guardian, metrics }
    }

    pub async fn verify(
        &self,
        problem: &str,
        outputs: &[AgentOutput],
        summaries: &[String],
    ) -> Result<VerificationResult> {
        let mut prompt = format!("## Original Problem\n{problem}\n\n## What was done\n");
        for (i, summary) in summaries.iter().enumerate() {
            prompt.push_str(&format!("Phase {}: {summary}\n\n", i + 1));
        }
        for output in outputs {
            prompt.push_str(&format!(
                "### Agent: {} (confidence: {:.0}%)\n{}\n\n",
                output.role, output.confidence * 100.0,
                &output.content[..output.content.len().min(1000)],
            ));
        }
        prompt.push_str(
            "\nNow VERIFY by running read-only commands. Check the current system state.\n\
             Use tool calls to verify, then conclude with your verdict as JSON."
        );

        // Verifier runs its own tool-call loop (read-only only)
        let mut conversation = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let agent_id = self.config.id.to_string();

        for _turn in 0..10 {
            let (response, _) = self.client
                .send_message(VERIFIER_SYSTEM_PROMPT, &conversation)
                .await?;

            conversation.push(ChatMessage {
                role: MessageRole::Assistant,
                content: response.clone(),
            });

            // Try to extract a tool call
            if let Some(tool_call) = extract_tool_call(&response) {
                tracing::info!(agent = "Verifier", tool = %tool_call.tool_name, "Verifier executing check");

                // Guardian review
                let decision = self.guardian
                    .review(&tool_call, "Verifier", &agent_id, &response)
                    .await?;

                match decision {
                    crate::safety::guardian::ReviewDecision::Approved { .. } => {
                        self.metrics.record_guardian_approval();
                        if let Ok(tool) = self.tools.get(&tool_call.tool_name) {
                            let result = tool.execute(&tool_call, Duration::from_secs(30)).await;
                            let result_text = match result {
                                Ok(r) if r.success => format!("Verification output:\n{}", &r.output[..r.output.len().min(2000)]),
                                Ok(r) => format!("Check failed:\n{}", r.error.unwrap_or_default()),
                                Err(e) => format!("Check error: {e}"),
                            };
                            conversation.push(ChatMessage {
                                role: MessageRole::User,
                                content: result_text,
                            });
                        }
                    }
                    _ => {
                        self.metrics.record_guardian_block();
                        conversation.push(ChatMessage {
                            role: MessageRole::User,
                            content: "Command blocked by Guardian. Try a different read-only check.".into(),
                        });
                    }
                }
            } else {
                // No tool call — this is the final verdict
                return Ok(VerificationResult::parse(&response));
            }
        }

        // Ran out of turns
        let last = conversation.iter().rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(VerificationResult::parse(&last))
    }
}

fn extract_tool_call(response: &str) -> Option<ToolParams> {
    let start = response.find('{')?;
    let mut depth = 0;
    let mut end = start;
    for (i, c) in response[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => { depth -= 1; if depth == 0 { end = start + i + 1; break; } }
            _ => {}
        }
    }
    if depth != 0 { return None; }

    let json_str = &response[start..end];
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = value.as_object()?;

    // Must have "tool" field to be a tool call (not a verification result)
    let tool_name = obj.get("tool")?.as_str()?.to_string();
    let action = obj.get("action")?.as_str()?.to_string();
    let args = obj.get("args")?.as_object()?
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();

    Some(ToolParams { tool_name, action, args })
}

#[async_trait]
impl AgentBehavior for VerifierAgent {
    fn config(&self) -> &AgentConfig { &self.config }
    async fn execute(&self, input: &str) -> Result<AgentOutput> {
        let messages = vec![ChatMessage { role: MessageRole::User, content: input.to_string() }];
        let (response, _) = self.client.send_message(VERIFIER_SYSTEM_PROMPT, &messages).await?;
        Ok(AgentOutput::new(self.config.id.clone(), "Verifier".into(), response))
    }
}

const VERIFIER_SYSTEM_PROMPT: &str = r#"You are a verification agent. Your job is to CHECK if a problem is actually resolved by running read-only commands.

IMPORTANT: Do NOT just analyze the text. Actually RUN commands to verify the current system state.

To run a command, output ONLY a JSON object:
{"tool": "shell", "action": "run", "args": {"command": "your read-only command"}}

Verification steps:
1. Run commands to check the current state (docker ps, kubectl get pods, systemctl status, curl, etc.)
2. Compare against what was expected
3. Check for regressions (new errors, services down that were up before)

After running your checks, provide your final verdict as JSON (WITHOUT a tool call):
{"status": "Resolved", "confidence": 0.95, "evidence": "..."}
{"status": "PartiallyResolved", "confidence": 0.5, "what_worked": "...", "what_remains": "..."}
{"status": "Failed", "reason": "...", "suggested_next_steps": "..."}
{"status": "Inconclusive", "reason": "..."}
{"status": "Regressed", "original_fixed": "...", "new_issue": "...", "evidence": "..."}

Only use read-only commands. Never modify the system."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resolved() {
        let json = r#"{"status": "Resolved", "confidence": 0.95, "evidence": "nginx returns 200"}"#;
        assert!(VerificationResult::parse(json).is_resolved());
    }

    #[test]
    fn parse_regressed() {
        let json = r#"{"status": "Regressed", "original_fixed": "502 fixed", "new_issue": "high latency", "evidence": "p99 > 5s"}"#;
        assert!(matches!(VerificationResult::parse(json), VerificationResult::Regressed { .. }));
    }

    #[test]
    fn parse_failed() {
        let json = r#"{"status": "Failed", "reason": "still 502", "suggested_next_steps": "check upstream"}"#;
        assert!(matches!(VerificationResult::parse(json), VerificationResult::Failed { .. }));
    }

    #[test]
    fn parse_fallback() {
        assert!(VerificationResult::parse("RESOLVED - all good").is_resolved());
        assert!(matches!(VerificationResult::parse("something unknown"), VerificationResult::Inconclusive { .. }));
    }
}
