use serde_json::Value;

use crate::api::types::{ChatMessage, MessageRole, Usage};
use crate::api::provider::LlmProvider;
use crate::error::{AgentError, Result};

/// Validates a JSON string against a JSON schema, retrying with feedback on failure.
///
/// If the response doesn't match the schema, sends the validation errors back to the
/// LLM as feedback, asking it to fix the output. Retries up to `max_retries` times.
pub async fn validate_and_retry<T: serde::de::DeserializeOwned>(
    client: &dyn LlmProvider,
    system_prompt: &str,
    original_messages: &[ChatMessage],
    response_text: &str,
    schema: &Value,
    max_retries: u32,
) -> Result<(T, Usage)> {
    let mut current_text = response_text.to_string();
    let mut total_usage = Usage::default();

    for attempt in 0..=max_retries {
        // Try to extract JSON from the response (may be wrapped in markdown)
        let json_str = extract_json(&current_text);

        // Parse as JSON value first
        let parsed: std::result::Result<Value, _> = serde_json::from_str(&json_str);
        let value = match parsed {
            Ok(v) => v,
            Err(e) => {
                if attempt == max_retries {
                    return Err(AgentError::SchemaValidation(format!(
                        "Invalid JSON after {max_retries} retries: {e}"
                    )));
                }
                let (retry_text, usage) = retry_with_feedback(
                    client,
                    system_prompt,
                    original_messages,
                    &current_text,
                    &format!("Your response is not valid JSON: {e}"),
                )
                .await?;
                total_usage.input_tokens += usage.input_tokens;
                total_usage.output_tokens += usage.output_tokens;
                current_text = retry_text;
                continue;
            }
        };

        // Validate against schema
        let validator = jsonschema::validator_for(schema)
            .map_err(|e| AgentError::SchemaValidation(format!("Invalid schema: {e}")))?;

        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| format!("- {e} at {}", e.instance_path))
            .collect();

        if errors.is_empty() {
            // Schema valid — deserialize into target type
            let result: T = serde_json::from_value(value)
                .map_err(|e| AgentError::SchemaValidation(format!("Deserialization failed: {e}")))?;
            return Ok((result, total_usage));
        }

        if attempt == max_retries {
            return Err(AgentError::SchemaValidation(format!(
                "Schema validation failed after {max_retries} retries:\n{}",
                errors.join("\n")
            )));
        }

        let feedback = format!(
            "Your JSON output has schema validation errors:\n{}\n\nPlease fix these errors and output valid JSON only.",
            errors.join("\n")
        );

        let (retry_text, usage) = retry_with_feedback(
            client,
            system_prompt,
            original_messages,
            &current_text,
            &feedback,
        )
        .await?;
        total_usage.input_tokens += usage.input_tokens;
        total_usage.output_tokens += usage.output_tokens;
        current_text = retry_text;
    }

    unreachable!()
}

async fn retry_with_feedback(
    client: &dyn LlmProvider,
    system_prompt: &str,
    original_messages: &[ChatMessage],
    previous_response: &str,
    feedback: &str,
) -> Result<(String, Usage)> {
    let mut messages = original_messages.to_vec();
    messages.push(ChatMessage {
        role: MessageRole::Assistant,
        content: previous_response.to_string(),
    });
    messages.push(ChatMessage {
        role: MessageRole::User,
        content: feedback.to_string(),
    });

    client.send_message(system_prompt, &messages).await
}

/// Extract JSON from a string that may be wrapped in markdown code blocks.
pub fn extract_json(text: &str) -> String {
    let trimmed = text.trim();

    // Try to find JSON in ```json ... ``` blocks
    if let Some(start) = trimmed.find("```json") {
        let after_marker = &trimmed[start + 7..];
        if let Some(end) = after_marker.find("```") {
            return after_marker[..end].trim().to_string();
        }
    }

    // Try to find JSON in ``` ... ``` blocks
    if let Some(start) = trimmed.find("```") {
        let after_marker = &trimmed[start + 3..];
        if let Some(end) = after_marker.find("```") {
            let content = after_marker[..end].trim();
            // Only use if it looks like JSON
            if content.starts_with('{') || content.starts_with('[') {
                return content.to_string();
            }
        }
    }

    // Return as-is if it already looks like JSON
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(extract_json(input), input);
    }

    #[test]
    fn extract_json_from_markdown() {
        let input = "Here is the plan:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_from_generic_code_block() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn extract_json_ignores_non_json_code_block() {
        let input = "```\nsome text\n```";
        // Falls through to return trimmed original since "some text" doesn't start with {
        assert_eq!(extract_json(input), input.trim());
    }
}
