use crate::error::{AgentError, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
    pub session_token_budget: u32,
    pub per_agent_token_budget: u32,
    pub per_agent_conversation_cap: u16,
    pub log_level: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        // API key: try provider-specific env var, fall back to ANTHROPIC_API_KEY
        // Not required for local provider
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .unwrap_or_default();

        Ok(Self {
            api_key,
            model: env_or("CLAUDE_MODEL", "claude-sonnet-4-20250514"),
            max_tokens: env_parse("MAX_TOKENS", 4096),
            base_url: env_or("API_BASE_URL", "https://api.anthropic.com"),
            session_token_budget: env_parse("SESSION_TOKEN_BUDGET", 2_000_000),
            per_agent_token_budget: env_parse("PER_AGENT_TOKEN_BUDGET", 400_000),
            per_agent_conversation_cap: env_parse("PER_AGENT_CONVERSATION_CAP", 50),
            log_level: env_or("LOG_LEVEL", "info"),
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_api_key_defaults_to_empty() {
        // API key is optional now (local provider doesn't need one)
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("GEMINI_API_KEY");
        };
        let result = Config::from_env();
        assert!(result.is_ok());
        assert!(result.unwrap().api_key.is_empty());
    }

    #[test]
    fn env_parse_defaults() {
        // Test the helper functions directly instead of relying on env var state
        assert_eq!(env_parse::<u32>("NONEXISTENT_VAR_12345", 4096), 4096);
        assert_eq!(env_or("NONEXISTENT_VAR_12345", "default"), "default");
    }
}
