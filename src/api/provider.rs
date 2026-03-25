use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::api::types::{ChatMessage, Usage};
use crate::error::Result;

/// Unified trait for all LLM providers (Claude, OpenAI, DeepSeek, Gemini, local).
///
/// Every component in opcrew uses `Arc<dyn LlmProvider>` instead of a concrete client.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a message and wait for the full response.
    async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)>;

    /// Send a message with retry logic (exponential backoff).
    async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)>;

    /// Stream a response, sending text chunks via the channel.
    async fn send_message_stream(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)>;

    /// Provider name for logging and display.
    fn provider_name(&self) -> &str;

    /// Model name currently in use.
    fn model_name(&self) -> &str;
}
