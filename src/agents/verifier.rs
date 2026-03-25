use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::api::provider::LlmProvider;
use crate::api::schema::extract_json;
use crate::api::types::{ChatMessage, MessageRole};
use crate::domain::agent::{AgentBehavior, AgentConfig, AgentId, AgentOutput};
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum VerificationResult {
    Resolved {
        confidence: f32,
        evidence: String,
    },
    PartiallyResolved {
        confidence: f32,
        what_worked: String,
        what_remains: String,
    },
    Failed {
        reason: String,
        suggested_next_steps: String,
    },
    Inconclusive {
        reason: String,
    },
    Regressed {
        original_fixed: String,
        new_issue: String,
        evidence: String,
    },
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
        // Fallback: try to infer from text
        let upper = response.to_uppercase();
        if upper.contains("RESOLVED") && !upper.contains("PARTIALLY") && !upper.contains("REGRESS") {
            VerificationResult::Resolved {
                confidence: 0.7,
                evidence: response.to_string(),
            }
        } else if upper.contains("REGRESS") {
            VerificationResult::Regressed {
                original_fixed: "Unknown".into(),
                new_issue: response.to_string(),
                evidence: response.to_string(),
            }
        } else if upper.contains("PARTIALLY") {
            VerificationResult::PartiallyResolved {
                confidence: 0.4,
                what_worked: "Unknown".into(),
                what_remains: response.to_string(),
            }
        } else if upper.contains("FAILED") || upper.contains("NOT RESOLVED") {
            VerificationResult::Failed {
                reason: response.to_string(),
                suggested_next_steps: String::new(),
            }
        } else {
            VerificationResult::Inconclusive {
                reason: response.to_string(),
            }
        }
    }
}

pub struct VerifierAgent {
    config: AgentConfig,
    client: Arc<dyn LlmProvider>,
}

impl VerifierAgent {
    pub fn new(client: Arc<dyn LlmProvider>) -> Self {
        let config = AgentConfig {
            id: AgentId::new(),
            role: "Verifier".to_string(),
            expertise: vec!["verification".into(), "regression testing".into()],
            system_prompt: VERIFIER_SYSTEM_PROMPT.to_string(),
            goal: "Verify if the problem is actually resolved".to_string(),
            allowed_tools: vec!["shell".into(), "log_reader".into()],
            token_budget: 50_000,
            max_conversation_turns: 10,
        };
        Self { config, client }
    }

    pub async fn verify(
        &self,
        problem: &str,
        outputs: &[AgentOutput],
        summaries: &[String],
    ) -> Result<VerificationResult> {
        let mut prompt = format!(
            "## Original Problem\n{problem}\n\n## What was done\n"
        );
        for (i, summary) in summaries.iter().enumerate() {
            prompt.push_str(&format!("Phase {}: {summary}\n\n", i + 1));
        }
        for output in outputs {
            prompt.push_str(&format!(
                "### Agent: {} (confidence: {:.0}%)\n{}\n\n",
                output.role,
                output.confidence * 100.0,
                &output.content[..output.content.len().min(1500)],
            ));
        }
        prompt.push_str(
            "\nVerify: Is the original problem resolved? Check for regressions.\n\
             Respond with JSON: {\"status\": \"Resolved|PartiallyResolved|Failed|Inconclusive|Regressed\", ...}"
        );

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _) = self
            .client
            .send_message(VERIFIER_SYSTEM_PROMPT, &messages)
            .await?;

        Ok(VerificationResult::parse(&response))
    }
}

#[async_trait]
impl AgentBehavior for VerifierAgent {
    fn config(&self) -> &AgentConfig {
        &self.config
    }

    async fn execute(&self, input: &str) -> Result<AgentOutput> {
        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: input.to_string(),
        }];
        let (response, _) = self
            .client
            .send_message(VERIFIER_SYSTEM_PROMPT, &messages)
            .await?;

        Ok(AgentOutput::new(
            self.config.id.clone(),
            "Verifier".into(),
            response,
        ))
    }
}

const VERIFIER_SYSTEM_PROMPT: &str = r#"You are a verification agent. Your job is to confirm whether a problem has actually been resolved.

After specialists have worked on a problem, you must:
1. Re-examine the current system state using read-only checks
2. Determine if the original problem is resolved
3. Check for REGRESSIONS — the fix may have broken something else

CRITICAL: Check for regressions:
- Are all services that were healthy before still healthy?
- Are there new errors in logs that weren't there before?
- Has performance degraded (response times, resource usage)?
- Are dependent services affected?

Respond with JSON in one of these formats:
{"status": "Resolved", "confidence": 0.95, "evidence": "..."}
{"status": "PartiallyResolved", "confidence": 0.5, "what_worked": "...", "what_remains": "..."}
{"status": "Failed", "reason": "...", "suggested_next_steps": "..."}
{"status": "Inconclusive", "reason": "..."}
{"status": "Regressed", "original_fixed": "...", "new_issue": "...", "evidence": "..."}

Be honest about confidence. Only say Resolved if you have strong evidence."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resolved() {
        let json = r#"{"status": "Resolved", "confidence": 0.95, "evidence": "nginx returns 200"}"#;
        let result = VerificationResult::parse(json);
        assert!(result.is_resolved());
    }

    #[test]
    fn parse_regressed() {
        let json = r#"{"status": "Regressed", "original_fixed": "502 fixed", "new_issue": "high latency", "evidence": "p99 > 5s"}"#;
        let result = VerificationResult::parse(json);
        assert!(matches!(result, VerificationResult::Regressed { .. }));
    }

    #[test]
    fn parse_failed() {
        let json = r#"{"status": "Failed", "reason": "still 502", "suggested_next_steps": "check upstream"}"#;
        let result = VerificationResult::parse(json);
        assert!(matches!(result, VerificationResult::Failed { .. }));
    }

    #[test]
    fn parse_partially_resolved() {
        let json = r#"{"status": "PartiallyResolved", "confidence": 0.5, "what_worked": "logs clean", "what_remains": "port still down"}"#;
        let result = VerificationResult::parse(json);
        assert!(matches!(result, VerificationResult::PartiallyResolved { .. }));
    }

    #[test]
    fn parse_inconclusive() {
        let json = r#"{"status": "Inconclusive", "reason": "cannot access system"}"#;
        let result = VerificationResult::parse(json);
        assert!(matches!(result, VerificationResult::Inconclusive { .. }));
    }

    #[test]
    fn fallback_text_parsing() {
        let result = VerificationResult::parse("The problem is RESOLVED, nginx returns 200 OK");
        assert!(result.is_resolved());

        let result = VerificationResult::parse("REGRESSED: nginx fixed but redis crashed");
        assert!(matches!(result, VerificationResult::Regressed { .. }));

        let result = VerificationResult::parse("Something unknown happened");
        assert!(matches!(result, VerificationResult::Inconclusive { .. }));
    }
}
