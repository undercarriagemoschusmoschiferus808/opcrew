use std::sync::RwLock;

use regex::Regex;

/// Proactive secret masking at all boundaries.
///
/// Maintains both static patterns and dynamically learned secrets
/// (from files read by agents, like .env).
pub struct SecretMasker {
    /// Dynamically learned secret values (from file reads, etc.)
    learned_secrets: RwLock<Vec<(String, String)>>, // (value, type)
}

impl SecretMasker {
    pub fn new() -> Self {
        Self {
            learned_secrets: RwLock::new(Vec::new()),
        }
    }

    /// Mask secrets in a string. Checks both static patterns and learned values.
    pub fn mask_string(&self, input: &str) -> String {
        let mut result = input.to_string();

        // First, mask learned secrets (exact match — highest priority)
        let learned = self.learned_secrets.read().unwrap();
        for (secret, secret_type) in learned.iter() {
            if secret.len() >= 8 {
                // Only mask non-trivial values
                result = result.replace(secret, &format!("[REDACTED:{secret_type}]"));
            }
        }
        drop(learned);

        // Static pattern matching
        result = mask_static_patterns(&result);

        result
    }

    /// Mask secrets in a serde_json::Value (recursive).
    pub fn mask_value(&self, value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => serde_json::Value::String(self.mask_string(s)),
            serde_json::Value::Object(map) => {
                let masked: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .map(|(k, v)| {
                        if is_secret_key(k) {
                            (
                                k.clone(),
                                serde_json::Value::String(format!("[REDACTED:{k}]")),
                            )
                        } else {
                            (k.clone(), self.mask_value(v))
                        }
                    })
                    .collect();
                serde_json::Value::Object(masked)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.iter().map(|v| self.mask_value(v)).collect())
            }
            other => other.clone(),
        }
    }

    /// Learn a secret value from content (e.g., when agent reads a .env file).
    /// Extracts KEY=VALUE pairs and stores the values for future masking.
    pub fn learn_from_env_content(&self, content: &str) {
        let mut learned = self.learned_secrets.write().unwrap();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if is_secret_key(key) && !value.is_empty() {
                    learned.push((value.to_string(), key.to_string()));
                }
            }
        }
    }

    /// Learn a specific secret value.
    pub fn learn_secret(&self, value: String, secret_type: String) {
        if value.len() >= 8 {
            let mut learned = self.learned_secrets.write().unwrap();
            learned.push((value, secret_type));
        }
    }
}

fn is_secret_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    upper.contains("KEY")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("PASSWD")
        || upper.contains("CREDENTIAL")
        || upper.contains("AUTH")
        || upper.ends_with("_PWD")
}

fn mask_static_patterns(input: &str) -> String {
    let mut result = input.to_string();

    // AWS access keys
    let aws_re = Regex::new(r"AKIA[0-9A-Z]{16}").unwrap();
    result = aws_re
        .replace_all(&result, "[REDACTED:aws_key]")
        .to_string();

    // Anthropic API keys
    let anthropic_re = Regex::new(r"sk-ant-[a-zA-Z0-9\-_]{20,}").unwrap();
    result = anthropic_re
        .replace_all(&result, "[REDACTED:anthropic_key]")
        .to_string();

    // OpenAI API keys
    let openai_re = Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap();
    result = openai_re
        .replace_all(&result, "[REDACTED:api_key]")
        .to_string();

    // GitHub tokens
    let gh_re = Regex::new(r"gh[ps]_[A-Za-z0-9_]{36,}").unwrap();
    result = gh_re
        .replace_all(&result, "[REDACTED:github_token]")
        .to_string();

    // Bearer tokens
    let bearer_re = Regex::new(r"Bearer\s+[A-Za-z0-9\-._~+/]+=*").unwrap();
    result = bearer_re
        .replace_all(&result, "Bearer [REDACTED:bearer_token]")
        .to_string();

    // Passwords in URLs
    let url_pass_re = Regex::new(r"://([^:]+):([^@]{3,})@").unwrap();
    result = url_pass_re
        .replace_all(&result, "://$1:[REDACTED:url_password]@")
        .to_string();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_aws_keys() {
        let input = "key: AKIAIOSFODNN7EXAMPLE";
        let masked = mask_static_patterns(input);
        assert!(masked.contains("[REDACTED:aws_key]"));
        assert!(!masked.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn masks_bearer_tokens() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.test";
        let masked = mask_static_patterns(input);
        assert!(masked.contains("[REDACTED:bearer_token]"));
    }

    #[test]
    fn masks_url_passwords() {
        let input = "postgres://admin:supersecretpass@db.example.com:5432/mydb";
        let masked = mask_static_patterns(input);
        assert!(masked.contains("[REDACTED:url_password]"));
        assert!(!masked.contains("supersecretpass"));
    }

    #[test]
    fn learns_from_env_content() {
        let masker = SecretMasker::new();
        masker.learn_from_env_content("API_KEY=my-very-secret-key-12345\nNOT_SECRET=hello");

        let masked = masker.mask_string("Using key: my-very-secret-key-12345");
        assert!(masked.contains("[REDACTED:API_KEY]"));
        assert!(!masked.contains("my-very-secret-key-12345"));
    }

    #[test]
    fn masks_json_values() {
        let masker = SecretMasker::new();
        let value = serde_json::json!({
            "command": "curl http://api.com",
            "api_key": "sk-secret-12345678",
            "password": "hunter2"
        });

        let masked = masker.mask_value(&value);
        let obj = masked.as_object().unwrap();
        assert!(obj["api_key"].as_str().unwrap().contains("REDACTED"));
        assert!(obj["password"].as_str().unwrap().contains("REDACTED"));
    }
}
