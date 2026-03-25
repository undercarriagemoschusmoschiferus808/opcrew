use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use governor::{Quota, RateLimiter};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, MessageRole, Usage};
use crate::config::Config;
use crate::error::{AgentError, Result};

type TokenBucket = RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

/// OpenAI-compatible client. Works with OpenAI, DeepSeek, and any OpenAI-compatible API.
pub struct OpenAiClient {
    http: Client,
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
    rate_limiter: Arc<TokenBucket>,
    provider: String,
}

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessage>,
    delta: Option<OpenAiDelta>,
}

#[derive(Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

impl OpenAiClient {
    pub fn new_openai(config: &Config, model_override: Option<String>) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| AgentError::ConfigError("OPENAI_API_KEY is required for --provider openai".into()))?;
        let model = model_override
            .or_else(|| std::env::var("OPENAI_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o".into());

        Ok(Self::build(api_key, "https://api.openai.com/v1/chat/completions".into(), model, config.max_tokens, "openai".into()))
    }

    pub fn new_deepseek(config: &Config, model_override: Option<String>) -> Result<Self> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| AgentError::ConfigError("DEEPSEEK_API_KEY is required for --provider deepseek".into()))?;
        let model = model_override
            .or_else(|| std::env::var("DEEPSEEK_MODEL").ok())
            .unwrap_or_else(|| "deepseek-chat".into());

        Ok(Self::build(api_key, "https://api.deepseek.com/chat/completions".into(), model, config.max_tokens, "deepseek".into()))
    }

    pub fn new_local(base_url: String, model: String, max_tokens: u32) -> Self {
        Self::build(String::new(), base_url, model, max_tokens, "local".into())
    }

    fn build(api_key: String, base_url: String, model: String, max_tokens: u32, provider: String) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        let quota = Quota::per_second(NonZeroU32::new(10).unwrap());
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Self {
            http,
            api_key,
            base_url,
            model,
            max_tokens,
            rate_limiter,
            provider,
        }
    }

    fn convert_messages(system_prompt: &str, messages: &[ChatMessage]) -> Vec<OpenAiMessage> {
        let mut out = Vec::new();
        if !system_prompt.is_empty() {
            out.push(OpenAiMessage {
                role: "system".into(),
                content: system_prompt.into(),
            });
        }
        for msg in messages {
            out.push(OpenAiMessage {
                role: match msg.role {
                    MessageRole::User => "user".into(),
                    MessageRole::Assistant => "assistant".into(),
                },
                content: msg.content.clone(),
            });
        }
        out
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiClient {
    async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)> {
        self.send_message_with_retries(system_prompt, messages, 3).await
    }

    async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)> {
        let oai_messages = Self::convert_messages(system_prompt, messages);
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let backoff = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                tokio::time::sleep(backoff).await;
            }

            self.rate_limiter.until_ready().await;

            let request = OpenAiRequest {
                model: self.model.clone(),
                max_tokens: self.max_tokens,
                messages: oai_messages.clone(),
                stream: None,
            };

            let mut req = self.http
                .post(&self.base_url)
                .header("Content-Type", "application/json");
            if !self.api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", self.api_key));
            }
            let response = req.json(&request).send().await?;

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

            let parsed: OpenAiResponse = serde_json::from_str(&body)
                .map_err(AgentError::SerializationError)?;

            let text = parsed.choices
                .first()
                .and_then(|c| c.message.as_ref())
                .map(|m| m.content.clone())
                .unwrap_or_default();

            let usage = parsed.usage
                .map(|u| Usage {
                    input_tokens: u.prompt_tokens,
                    output_tokens: u.completion_tokens,
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
        chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)> {
        self.rate_limiter.until_ready().await;

        let oai_messages = Self::convert_messages(system_prompt, messages);
        let request = OpenAiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: oai_messages,
            stream: Some(true),
        };

        let mut req = self.http
            .post(&self.base_url)
            .header("Content-Type", "application/json");
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let response = req.json(&request).send().await?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AgentError::ApiError {
                status_code: 0,
                message: body,
            });
        }

        let mut stream = response.bytes_stream();
        let mut accumulated = String::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(AgentError::RequestError)?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            while let Some(end) = buffer.find("\n\n") {
                let event = buffer[..end].to_string();
                buffer = buffer[end + 2..].to_string();

                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            continue;
                        }
                        if let Some(content) = serde_json::from_str::<OpenAiResponse>(data).ok()
                            .and_then(|p| p.choices.into_iter().next())
                            .and_then(|c| c.delta)
                            .and_then(|d| d.content)
                        {
                            accumulated.push_str(&content);
                            let _ = chunk_tx.send(content).await;
                        }
                    }
                }
            }
        }

        Ok((accumulated, Usage::default()))
    }

    fn provider_name(&self) -> &str {
        &self.provider
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
