use tokio::sync::mpsc;

use crate::api::openai::OpenAiClient;
use crate::api::provider::LlmProvider;
use crate::api::types::{ChatMessage, Usage};
use crate::config::Config;
use crate::error::Result;

/// Local LLM client (Ollama, llama.cpp, vLLM — anything OpenAI-compatible).
pub struct LocalClient {
    inner: OpenAiClient,
}

impl LocalClient {
    pub fn new(config: &Config) -> Self {
        let base_url = std::env::var("LOCAL_LLM_URL")
            .unwrap_or_else(|_| "http://localhost:11434/v1/chat/completions".into());
        let model = std::env::var("LOCAL_LLM_MODEL")
            .unwrap_or_else(|_| "llama3".into());

        let inner = OpenAiClient::new_local(base_url, model, config.max_tokens);
        Self { inner }
    }
}

#[async_trait::async_trait]
impl LlmProvider for LocalClient {
    async fn send_message(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(String, Usage)> {
        self.inner.send_message(system_prompt, messages).await
    }

    async fn send_message_with_retries(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        max_retries: u32,
    ) -> Result<(String, Usage)> {
        self.inner.send_message_with_retries(system_prompt, messages, max_retries).await
    }

    async fn send_message_stream(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        chunk_tx: mpsc::Sender<String>,
    ) -> Result<(String, Usage)> {
        self.inner.send_message_stream(system_prompt, messages, chunk_tx).await
    }

    fn provider_name(&self) -> &str {
        "local"
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
}
