use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub problem_statement: String,
    pub analysis: String,
    pub roles: Vec<PlannedRole>,
    pub tasks: Vec<PlannedTask>,
    pub synthesis_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedRole {
    pub role_name: String,
    pub expertise: Vec<String>,
    pub responsibility: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    #[serde(default)]
    pub target_host: Option<String>,
}

fn default_token_budget() -> u32 {
    100_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTask {
    pub title: String,
    pub description: String,
    pub assigned_role: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Link to a hypothesis ID (e.g., "H1: upstream server is down")
    #[serde(default)]
    pub hypothesis: Option<String>,
}

fn default_priority() -> u8 {
    1
}

/// JSON Schema for validating CEO plan output.
pub fn plan_json_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["analysis", "roles", "tasks", "synthesis_strategy"],
        "properties": {
            "analysis": {
                "type": "string",
                "minLength": 10
            },
            "roles": {
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "items": {
                    "type": "object",
                    "required": ["role_name", "expertise", "responsibility"],
                    "properties": {
                        "role_name": { "type": "string" },
                        "expertise": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1
                        },
                        "responsibility": { "type": "string" },
                        "allowed_tools": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "token_budget": { "type": "integer", "minimum": 1000 },
                        "target_host": { "type": ["string", "null"] }
                    }
                }
            },
            "tasks": {
                "type": "array",
                "minItems": 1,
                "maxItems": 10,
                "items": {
                    "type": "object",
                    "required": ["title", "description", "assigned_role"],
                    "properties": {
                        "title": { "type": "string" },
                        "description": { "type": "string" },
                        "assigned_role": { "type": "string" },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "priority": { "type": "integer", "minimum": 1, "maximum": 10 },
                        "hypothesis": { "type": ["string", "null"] }
                    }
                }
            },
            "synthesis_strategy": { "type": "string" }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_serialization_roundtrip() {
        let plan = Plan {
            problem_statement: "nginx 502".into(),
            analysis: "The server is returning 502 errors".into(),
            roles: vec![PlannedRole {
                role_name: "Log Analyst".into(),
                expertise: vec!["log analysis".into()],
                responsibility: "Analyze nginx logs".into(),
                allowed_tools: vec!["shell".into(), "log_reader".into()],
                token_budget: 50_000,
                target_host: None,
            }],
            tasks: vec![PlannedTask {
                title: "Check nginx error log".into(),
                description: "Read /var/log/nginx/error.log".into(),
                assigned_role: "Log Analyst".into(),
                depends_on: vec![],
                priority: 1,
                hypothesis: Some("H1: upstream server is down".into()),
            }],
            synthesis_strategy: "Combine findings into diagnosis".into(),
        };

        let json = serde_json::to_string_pretty(&plan).unwrap();
        let parsed: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.roles.len(), 1);
        assert_eq!(parsed.tasks.len(), 1);
    }

    #[test]
    fn schema_is_valid_json_schema() {
        let schema = plan_json_schema();
        // Verify the schema itself is valid by attempting to build a validator
        let result = jsonschema::validator_for(&schema);
        assert!(result.is_ok());
    }
}
