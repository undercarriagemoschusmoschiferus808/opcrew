use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AgentId(pub Uuid);

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.0.to_string()[..8])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: AgentId,
    pub role: String,
    pub expertise: Vec<String>,
    pub system_prompt: String,
    pub goal: String,
    pub allowed_tools: Vec<String>,
    pub token_budget: u32,
    pub max_conversation_turns: u16,
}

#[async_trait]
pub trait AgentBehavior: Send + Sync {
    fn config(&self) -> &AgentConfig;

    async fn execute(&self, input: &str) -> Result<AgentOutput>;

    fn role(&self) -> &str {
        &self.config().role
    }

    fn id(&self) -> &AgentId {
        &self.config().id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub agent_id: AgentId,
    pub role: String,
    pub content: String,
    pub confidence: f32,
    pub tokens_used: u32,
    pub metadata: HashMap<String, String>,
}

impl AgentOutput {
    pub fn new(agent_id: AgentId, role: String, content: String) -> Self {
        Self {
            agent_id,
            role,
            content,
            confidence: 0.0,
            tokens_used: 0,
            metadata: HashMap::new(),
        }
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub fn with_tokens(mut self, tokens: u32) -> Self {
        self.tokens_used = tokens;
        self
    }
}

// --- Signals for mid-execution CEO communication ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signal {
    UnexpectedFinding {
        agent_role: String,
        finding: String,
        severity: SignalSeverity,
    },
    HypothesisDenied {
        agent_role: String,
        hypothesis_id: String,
        evidence: String,
    },
    HypothesisConfirmed {
        agent_role: String,
        hypothesis_id: String,
        evidence: String,
    },
    RequestHelp {
        agent_role: String,
        question: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SignalSeverity {
    Info,
    Warning,
    Critical,
}

impl Signal {
    pub fn is_critical(&self) -> bool {
        match self {
            Signal::UnexpectedFinding { severity, .. } => *severity == SignalSeverity::Critical,
            _ => false,
        }
    }
}

// --- CEO Clarity Assessment ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "clear")]
pub enum ClarityAssessment {
    #[serde(rename = "true")]
    ClearEnough { reasoning: String },
    #[serde(rename = "false")]
    NeedsClarification {
        questions: Vec<String>,
        reasoning: String,
    },
}

impl ClarityAssessment {
    pub fn parse(response: &str) -> Self {
        let json_str = crate::api::schema::extract_json(response);
        if let Ok(assessment) = serde_json::from_str::<ClarityAssessment>(&json_str) {
            return assessment;
        }
        // Fallback: if parsing fails, assume clear enough
        ClarityAssessment::ClearEnough {
            reasoning: "Could not parse assessment, proceeding".into(),
        }
    }

    pub fn needs_clarification(&self) -> bool {
        matches!(self, Self::NeedsClarification { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_display_is_short() {
        let id = AgentId::new();
        let display = format!("{id}");
        assert_eq!(display.len(), 8);
    }

    #[test]
    fn agent_output_confidence_clamped() {
        let output =
            AgentOutput::new(AgentId::new(), "test".into(), "content".into()).with_confidence(1.5);
        assert_eq!(output.confidence, 1.0);

        let output =
            AgentOutput::new(AgentId::new(), "test".into(), "content".into()).with_confidence(-0.5);
        assert_eq!(output.confidence, 0.0);
    }

    #[test]
    fn agent_config_serialization_roundtrip() {
        let config = AgentConfig {
            id: AgentId::new(),
            role: "Developer".into(),
            expertise: vec!["Rust".into(), "Python".into()],
            system_prompt: "You are a developer".into(),
            goal: "Fix bugs".into(),
            allowed_tools: vec!["shell".into(), "file_ops".into()],
            token_budget: 100_000,
            max_conversation_turns: 30,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, "Developer");
        assert_eq!(parsed.allowed_tools.len(), 2);
    }
}
