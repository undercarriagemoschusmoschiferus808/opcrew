use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use governor::{Quota, RateLimiter};
use reqwest::Client;
use tokio::sync::mpsc;

use crate::api::types::{
    ChatMessage, ClaudeRequest, ClaudeResponse, StreamEvent, Usage,
};
use crate::config::Config;
use crate::error::{AgentError, Result};

type TokenBucket = RateLimiter<
    governor::state::NotKeyed,
    governor::state::InMemoryState,
    governor::clock::DefaultClock,
>;

pub struct ClaudeClient {
    http: Client,
    config: Arc<Config>,
    rate_limiter: Arc<TokenBucket>,
}

impl ClaudeClient {
    pub fn new(config: Arc<Config>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        // Token bucket: 10 requests per second max to avoid burst 429s
        let quota = Quota::per_second(NonZeroU32::new(10).unwrap());
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Self {
            http,
            config,
            rate_limiter,
        }
    }

    pub async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)> {
        self.send_message_with_retries(system_prompt, messages, 3)
            .await
    }

    pub async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)> {
        let mut last_error = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let backoff = Duration::from_millis(1000 * 2u64.pow(attempt - 1));
                let jitter = Duration::from_millis(rand_jitter());
                tokio::time::sleep(backoff + jitter).await;
            }

            // Wait for rate limiter
            self.rate_limiter
                .until_ready()
                .await;

            match self.send_request(system_prompt, messages, false).await {
                Ok(response) => {
                    let text = response.text();
                    let usage = response.usage.clone();
                    return Ok((text, usage));
                }
                Err(AgentError::ApiError {
                    status_code,
                    ref message,
                }) if is_retryable(status_code) => {
                    tracing::warn!(
                        attempt,
                        status_code,
                        message,
                        "Retryable API error"
                    );
                    last_error = Some(AgentError::ApiError {
                        status_code,
                        message: message.clone(),
                    });
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_error.unwrap_or_else(|| {
            AgentError::ApiError {
                status_code: 0,
                message: "All retries exhausted".into(),
            }
        }))
    }

    /// Stream a response, sending text chunks via the provided sender.
    /// Returns total accumulated text and usage on completion.
    pub async fn send_message_stream(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)> {
        self.rate_limiter.until_ready().await;

        let request = self.build_request(system_prompt, messages, true);
        let response = self
            .http
            .post(self.messages_url())
            .headers(self.build_headers())
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AgentError::ApiError {
                status_code: status.as_u16(),
                message: body,
            });
        }

        let mut stream = response.bytes_stream();
        let mut accumulated_text = String::new();
        let mut usage = Usage::default();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(AgentError::RequestError)?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Parse SSE events from buffer
            while let Some(event_end) = buffer.find("\n\n") {
                let event_str = buffer[..event_end].to_string();
                buffer = buffer[event_end + 2..].to_string();

                if let Some(data) = extract_sse_data(&event_str) {
                    if data == "[DONE]" {
                        continue;
                    }
                    match serde_json::from_str::<StreamEvent>(&data) {
                        Ok(StreamEvent::ContentBlockDelta { delta, .. }) => {
                            accumulated_text.push_str(&delta.text);
                            let _ = chunk_tx.send(delta.text).await;
                        }
                        Ok(StreamEvent::MessageDelta {
                            usage: delta_usage, ..
                        }) => {
                            usage.output_tokens += delta_usage.output_tokens;
                        }
                        Ok(StreamEvent::MessageStart { message }) => {
                            usage.input_tokens = message.usage.input_tokens;
                        }
                        Ok(StreamEvent::Error { error }) => {
                            return Err(AgentError::ApiError {
                                status_code: 0,
                                message: error.message,
                            });
                        }
                        Ok(_) => {} // ping, content_block_start/stop, message_stop
                        Err(e) => {
                            tracing::debug!(data, error = %e, "Failed to parse SSE event");
                        }
                    }
                }
            }
        }

        Ok((accumulated_text, usage))
    }

    async fn send_request(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        stream: bool,
    ) -> Result<ClaudeResponse> {
        let request = self.build_request(system_prompt, messages, stream);

        let response = self
            .http
            .post(self.messages_url())
            .headers(self.build_headers())
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(AgentError::ApiError {
                status_code: status.as_u16(),
                message: body,
            });
        }

        serde_json::from_str(&body).map_err(AgentError::SerializationError)
    }

    fn build_request(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        stream: bool,
    ) -> ClaudeRequest {
        ClaudeRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            system: if system_prompt.is_empty() {
                None
            } else {
                Some(system_prompt.to_string())
            },
            messages: messages.to_vec(),
            stream: if stream { Some(true) } else { None },
        }
    }

    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-api-key", self.config.api_key.parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.config.base_url)
    }
}

// --- LlmProvider trait implementation for ClaudeClient ---

#[async_trait::async_trait]
impl crate::api::provider::LlmProvider for ClaudeClient {
    async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)> {
        ClaudeClient::send_message(self, system_prompt, messages).await
    }

    async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)> {
        ClaudeClient::send_message_with_retries(self, system_prompt, messages, max_retries).await
    }

    async fn send_message_stream(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)> {
        ClaudeClient::send_message_stream(self, system_prompt, messages, chunk_tx).await
    }

    fn provider_name(&self) -> &str {
        "claude"
    }

    fn model_name(&self) -> &str {
        &self.config.model
    }
}

fn is_retryable(status_code: u16) -> bool {
    status_code == 429 || (500..600).contains(&status_code)
}

fn extract_sse_data(event_str: &str) -> Option<String> {
    for line in event_str.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            return Some(data.to_string());
        }
    }
    None
}

fn rand_jitter() -> u64 {
    // Simple deterministic jitter based on time to avoid rand dependency
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos % 500) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_status_codes() {
        assert!(is_retryable(429));
        assert!(is_retryable(500));
        assert!(is_retryable(502));
        assert!(is_retryable(503));
        assert!(!is_retryable(400));
        assert!(!is_retryable(401));
        assert!(!is_retryable(200));
    }

    #[test]
    fn sse_data_extraction() {
        assert_eq!(
            extract_sse_data("event: content_block_delta\ndata: {\"type\":\"text\"}"),
            Some("{\"type\":\"text\"}".to_string())
        );
        assert_eq!(extract_sse_data("no data here"), None);
        assert_eq!(
            extract_sse_data("data: [DONE]"),
            Some("[DONE]".to_string())
        );
    }
}
