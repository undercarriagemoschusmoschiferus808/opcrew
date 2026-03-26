use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::{Quota, RateLimiter};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, MessageRole, Usage};
use crate::config::Config;
use crate::error::{AgentError, Result};
// AgentError used for ConfigError in new()

type TokenBucket = RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

pub struct GeminiClient {
    http: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
    rate_limiter: Arc<TokenBucket>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    generation_config: GenerationConfig,
}

#[derive(Clone, Serialize, Deserialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Clone, Serialize, Deserialize)]
struct GeminiPart {
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    max_output_tokens: u32,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
}

impl GeminiClient {
    pub fn new(config: &Config, model_override: Option<String>) -> Result<Self> {
        let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| {
            AgentError::ConfigError("GEMINI_API_KEY is required for --provider gemini".into())
        })?;
        let model = model_override
            .or_else(|| std::env::var("GEMINI_MODEL").ok())
            .unwrap_or_else(|| "gemini-2.5-flash".into());

        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        let quota = Quota::per_second(NonZeroU32::new(10).unwrap());

        Ok(Self {
            http,
            api_key,
            model,
            max_tokens: config.max_tokens,
            rate_limiter: Arc::new(RateLimiter::direct(quota)),
        })
    }

    fn endpoint(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        )
    }

    fn convert_messages(
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> (Option<GeminiContent>, Vec<GeminiContent>) {
        let system = if system_prompt.is_empty() {
            None
        } else {
            Some(GeminiContent {
                role: "user".into(),
                parts: vec![GeminiPart {
                    text: system_prompt.into(),
                }],
            })
        };

        let contents: Vec<GeminiContent> = messages
            .iter()
            .map(|m| GeminiContent {
                role: match m.role {
                    MessageRole::User => "user".into(),
                    MessageRole::Assistant => "model".into(),
                },
                parts: vec![GeminiPart {
                    text: m.content.clone(),
                }],
            })
            .collect();

        (system, contents)
    }
}

#[async_trait::async_trait]
impl LlmProvider for GeminiClient {
    async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)> {
        self.send_message_with_retries(system_prompt, messages, 3)
            .await
    }

    async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)> {
        let (system, contents) = Self::convert_messages(system_prompt, messages);
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(1000 * 2u64.pow(attempt - 1))).await;
            }

            self.rate_limiter.until_ready().await;

            let request = GeminiRequest {
                contents: contents.clone(),
                system_instruction: system.clone(),
                generation_config: GenerationConfig {
                    max_output_tokens: self.max_tokens,
                },
            };

            let response = self
                .http
                .post(self.endpoint())
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await?;

            let status = response.status();
            let body = response.text().await?;

            if !status.is_success() {
                if status.as_u16() == 429 || (500..600).contains(&status.as_u16()) {
                    last_error = Some(AgentError::ApiError {
                        status_code: status.as_u16(),
                        message: body,
                    });
                    continue;
                }
                return Err(AgentError::ApiError {
                    status_code: status.as_u16(),
                    message: body,
                });
            }

            let parsed: GeminiResponse =
                serde_json::from_str(&body).map_err(AgentError::SerializationError)?;

            let text = parsed
                .candidates
                .as_ref()
                .and_then(|c| c.first())
                .map(|c| {
                    c.content
                        .parts
                        .iter()
                        .map(|p| p.text.as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();

            let usage = parsed
                .usage_metadata
                .map(|u| Usage {
                    input_tokens: u.prompt_token_count.unwrap_or(0),
                    output_tokens: u.candidates_token_count.unwrap_or(0),
                })
                .unwrap_or_default();

            return Ok((text, usage));
        }

        Err(last_error.unwrap_or(AgentError::ApiError {
            status_code: 0,
            message: "All retries exhausted".into(),
        }))
    }

    async fn send_message_stream(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        _chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)> {
        // Gemini streaming uses a different endpoint — fall back to non-streaming for now
        self.send_message(system_prompt, messages).await
    }

    fn provider_name(&self) -> &str {
        "gemini"
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
