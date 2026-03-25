use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::api::provider::LlmProvider;
use crate::api::schema::validate_and_retry;
use crate::api::types::{ChatMessage, MessageRole};
use crate::error::Result;
use crate::memory::models::HypothesisOutcome;
use crate::memory::store::MemoryStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypothesisReport {
    pub hypotheses: Vec<Hypothesis>,
    pub recommended_first_checks: Vec<String>,
    pub estimated_complexity: Complexity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub description: String,
    pub probability: f32,
    pub confirm_by: String,
    pub deny_by: String,
    pub fix_approach: String,
    #[serde(default)]
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Complexity {
    Simple,
    Moderate,
    Complex,
}

pub struct HypothesisAgent {
    client: Arc<dyn LlmProvider>,
}

impl HypothesisAgent {
    pub fn new(client: Arc<dyn LlmProvider>) -> Self {
        Self { client }
    }

    pub async fn generate(
        &self,
        problem: &str,
        memory_context: &str,
        infra_context: &str,
        priors: &[HypothesisOutcome],
    ) -> Result<HypothesisReport> {
        let mut prompt = format!("Problem to analyze:\n{problem}\n\n");

        if !memory_context.is_empty() {
            prompt.push_str(&format!("PAST EXPERIENCE:\n{memory_context}\n\n"));
        }

        if !infra_context.is_empty() {
            prompt.push_str(&format!("INFRASTRUCTURE CONTEXT:\n{infra_context}\n\n"));
        }

        if !priors.is_empty() {
            prompt.push_str("HISTORICAL DATA for this type of problem:\n");
            for prior in priors {
                prompt.push_str(&format!(
                    "- \"{}\": confirmed {}/{} times ({:.0}% prior)\n",
                    prior.hypothesis_category,
                    prior.times_confirmed,
                    prior.times_confirmed + prior.times_denied,
                    prior.prior_probability() * 100.0,
                ));
            }
            prompt.push_str("Use these priors to calibrate your probability estimates.\n\n");
        }

        prompt.push_str("Generate a structured hypothesis report. Respond with ONLY valid JSON.");

        let messages = vec![ChatMessage {
            role: MessageRole::User,
            content: prompt,
        }];

        let (response, _) = self
            .client
            .send_message(HYPOTHESIS_SYSTEM_PROMPT, &messages)
            .await?;

        let schema = hypothesis_report_schema();
        let (report, _): (HypothesisReport, _) = validate_and_retry(
            self.client.as_ref(),
            HYPOTHESIS_SYSTEM_PROMPT,
            &messages,
            &response,
            &schema,
            2,
        )
        .await?;

        Ok(report)
    }

    /// Format hypothesis report as context for the CEO prompt.
    pub fn format_for_ceo(report: &HypothesisReport) -> String {
        let mut out = String::new();
        out.push_str("HYPOTHESIS REPORT (from pre-analysis):\n\n");
        out.push_str(&format!(
            "Estimated complexity: {:?}\n\n",
            report.estimated_complexity
        ));

        for h in &report.hypotheses {
            out.push_str(&format!(
                "{} ({:.0}%): {}\n  Confirm by: {}\n  Deny by: {}\n  Fix: {}\n\n",
                h.id,
                h.probability * 100.0,
                h.description,
                h.confirm_by,
                h.deny_by,
                h.fix_approach,
            ));
        }

        if !report.recommended_first_checks.is_empty() {
            out.push_str("Recommended first checks:\n");
            for check in &report.recommended_first_checks {
                out.push_str(&format!("  - {check}\n"));
            }
        }

        out
    }
}

fn hypothesis_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["hypotheses", "recommended_first_checks", "estimated_complexity"],
        "properties": {
            "hypotheses": {
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "items": {
                    "type": "object",
                    "required": ["id", "description", "probability", "confirm_by", "deny_by", "fix_approach"],
                    "properties": {
                        "id": { "type": "string" },
                        "description": { "type": "string" },
                        "probability": { "type": "number", "minimum": 0, "maximum": 1 },
                        "confirm_by": { "type": "string" },
                        "deny_by": { "type": "string" },
                        "fix_approach": { "type": "string" },
                        "category": { "type": "string" }
                    }
                }
            },
            "recommended_first_checks": {
                "type": "array",
                "items": { "type": "string" }
            },
            "estimated_complexity": {
                "type": "string",
                "enum": ["Simple", "Moderate", "Complex"]
            }
        }
    })
}

const HYPOTHESIS_SYSTEM_PROMPT: &str = r#"You are a senior SRE with 15 years of experience. You receive a problem description and must generate a structured hypothesis report BEFORE any agents are deployed.

Your goal: maximize diagnostic efficiency by identifying the most likely causes first, so agents test in the right order and don't waste time on unlikely causes.

For each hypothesis:
- Assign a realistic probability based on frequency of this failure mode in production
- Specify the EXACT command or check that would confirm it (be precise)
- Specify what evidence would DENY it (equally important)
- Suggest a fix approach IF this hypothesis is confirmed
- Assign a category for tracking (e.g., "upstream_down", "config_error", "resource_exhaustion")

Order hypotheses by probability descending.
Focus on hypotheses that can be confirmed/denied with READ-ONLY checks first.

Respond with ONLY valid JSON matching this schema:
{
  "hypotheses": [
    {
      "id": "H1",
      "description": "Upstream application server is down",
      "probability": 0.6,
      "confirm_by": "curl -s http://localhost:3000/health returns non-200 or times out",
      "deny_by": "curl returns 200 OK within 1 second",
      "fix_approach": "Restart the application server: systemctl restart app",
      "category": "upstream_down"
    }
  ],
  "recommended_first_checks": ["check if upstream port is listening", "read last 50 lines of nginx error log"],
  "estimated_complexity": "Simple"
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hypothesis_report_schema_valid() {
        let schema = hypothesis_report_schema();
        let result = jsonschema::validator_for(&schema);
        assert!(result.is_ok());
    }

    #[test]
    fn hypothesis_report_deserialization() {
        let json = r#"{
            "hypotheses": [{
                "id": "H1",
                "description": "upstream down",
                "probability": 0.7,
                "confirm_by": "curl test",
                "deny_by": "curl returns 200",
                "fix_approach": "restart",
                "category": "upstream_down"
            }],
            "recommended_first_checks": ["check port"],
            "estimated_complexity": "Simple"
        }"#;

        let report: HypothesisReport = serde_json::from_str(json).unwrap();
        assert_eq!(report.hypotheses.len(), 1);
        assert_eq!(report.hypotheses[0].id, "H1");
        assert_eq!(report.estimated_complexity, Complexity::Simple);
    }

    #[test]
    fn format_for_ceo_includes_all_hypotheses() {
        let report = HypothesisReport {
            hypotheses: vec![
                Hypothesis {
                    id: "H1".into(),
                    description: "upstream down".into(),
                    probability: 0.7,
                    confirm_by: "curl test".into(),
                    deny_by: "curl 200".into(),
                    fix_approach: "restart".into(),
                    category: "upstream_down".into(),
                },
                Hypothesis {
                    id: "H2".into(),
                    description: "config error".into(),
                    probability: 0.2,
                    confirm_by: "nginx -t".into(),
                    deny_by: "config ok".into(),
                    fix_approach: "fix config".into(),
                    category: "config_error".into(),
                },
            ],
            recommended_first_checks: vec!["check port".into()],
            estimated_complexity: Complexity::Moderate,
        };

        let formatted = HypothesisAgent::format_for_ceo(&report);
        assert!(formatted.contains("H1"));
        assert!(formatted.contains("H2"));
        assert!(formatted.contains("70%"));
        assert!(formatted.contains("Moderate"));
    }

    #[test]
    fn complexity_serialization() {
        let c = Complexity::Complex;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"Complex\"");
        let parsed: Complexity = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Complexity::Complex);
    }
}
