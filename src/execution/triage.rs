use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::api::provider::LlmProvider;
use crate::api::schema::extract_json;
use crate::api::types::{ChatMessage, MessageRole};
use crate::error::Result;
use crate::execution::prefetch::SystemContext;

/// Result of the triage LLM call — diagnosis + fix in one shot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageResult {
    pub diagnostic: String,
    pub root_cause: String,
    pub confidence: f32,
    #[serde(default)]
    pub fix_commands: Vec<String>,
    #[serde(default)]
    pub verify_commands: Vec<String>,
    #[serde(default)]
    pub need_more_info: Vec<String>,
}

impl TriageResult {
    pub fn is_confident(&self) -> bool {
        self.confidence >= 0.8 && !self.fix_commands.is_empty()
    }

    pub fn needs_deeper_investigation(&self) -> bool {
        self.confidence < 0.8 || !self.need_more_info.is_empty()
    }

    pub fn parse(response: &str) -> Option<Self> {
        let json_str = extract_json(response);
        serde_json::from_str(&json_str).ok()
    }
}

/// Single LLM call that analyzes all pre-fetched data and returns diagnosis + fix.
pub async fn triage(
    client: &Arc<dyn LlmProvider>,
    problem: &str,
    system_context: &SystemContext,
) -> Result<TriageResult> {
    let context = system_context.to_prompt_context();

    let prompt = format!(
        "Problem reported: {problem}\n\n\
         {context}\n\
         Analyze the system data above and respond with ONLY valid JSON:\n\
         {{\n\
           \"diagnostic\": \"what you found\",\n\
           \"root_cause\": \"the specific root cause\",\n\
           \"confidence\": 0.0-1.0,\n\
           \"fix_commands\": [\"exact shell command to fix\"],\n\
           \"verify_commands\": [\"command to verify the fix worked\"],\n\
           \"need_more_info\": [\"commands to run if you need more data\"]\n\
         }}\n\n\
         Rules:\n\
         - confidence 0.8+ means you're sure of the root cause AND have a fix\n\
         - fix_commands: exact commands, no pipes, no &&, atomic only\n\
         - if the data doesn't show the problem clearly, set confidence < 0.8 and fill need_more_info\n\
         - verify_commands: read-only commands to confirm the fix worked"
    );

    let messages = vec![ChatMessage {
        role: MessageRole::User,
        content: prompt,
    }];

    let (response, _) = client.send_message(TRIAGE_SYSTEM_PROMPT, &messages).await?;

    match TriageResult::parse(&response) {
        Some(result) => Ok(result),
        None => {
            // Fallback: not confident, need full pipeline
            Ok(TriageResult {
                diagnostic: response[..response.len().min(500)].to_string(),
                root_cause: "Could not parse triage result".into(),
                confidence: 0.0,
                fix_commands: vec![],
                verify_commands: vec![],
                need_more_info: vec!["Full investigation needed".into()],
            })
        }
    }
}

const TRIAGE_SYSTEM_PROMPT: &str = r#"You are an expert SRE triaging an infrastructure incident. You receive pre-fetched system data and must diagnose the problem in ONE response.

Be direct:
- Look at the data. Find the error. Name the root cause.
- If you see the problem clearly: give exact fix commands (confidence >= 0.8)
- If the data doesn't show the problem: say what additional data you need (confidence < 0.8)
- DO NOT ask to run commands you already have output for

Respond with ONLY valid JSON. No explanation outside the JSON."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_confident_result() {
        let json = r#"{"diagnostic": "Container crashes due to missing PulseAudio socket", "root_cause": "PulseAudio socket not mounted", "confidence": 0.95, "fix_commands": ["docker stop song-stream-server", "docker run -v /run/user/1000/pulse:/run/user/1000/pulse song-stream-server"], "verify_commands": ["docker ps --filter name=song-stream-server"], "need_more_info": []}"#;
        let result = TriageResult::parse(json).unwrap();
        assert!(result.is_confident());
        assert!(!result.needs_deeper_investigation());
        assert_eq!(result.fix_commands.len(), 2);
    }

    #[test]
    fn parse_low_confidence_result() {
        let json = r#"{"diagnostic": "App is slow but no obvious cause", "root_cause": "Unknown", "confidence": 0.3, "fix_commands": [], "verify_commands": [], "need_more_info": ["Check application logs", "Run slow query analysis"]}"#;
        let result = TriageResult::parse(json).unwrap();
        assert!(!result.is_confident());
        assert!(result.needs_deeper_investigation());
    }
}
