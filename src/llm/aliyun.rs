//! Aliyun Coding Plan provider implementation.
//!
//! This provider is specifically designed for Aliyun's Coding Plan service
//! which uses Anthropic API compatible interface but requires specific HTTP configuration.

use async_trait::async_trait;
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use rust_decimal::Decimal;
use secrecy::ExposeSecret;

use crate::llm::config::AliyunConfig;
use crate::llm::costs;
use crate::llm::error::LlmError;
use crate::llm::provider::{
    ChatMessage, CompletionRequest, CompletionResponse, FinishReason, LlmProvider, Role, ToolCall,
    ToolCompletionRequest, ToolCompletionResponse, ToolDefinition,
};

/// Aliyun Coding Plan provider.
pub struct AliyunProvider {
    client: Client,
    config: AliyunConfig,
    active_model: std::sync::RwLock<String>,
}

impl AliyunProvider {
    /// Create a new Aliyun provider.
    pub fn new(config: AliyunConfig) -> Result<Self, LlmError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            // Use HTTP/1.1 to avoid potential HTTP/2 issues with Aliyun
            .http1_only()
            .build()
            .map_err(|e| LlmError::RequestFailed {
                provider: "aliyun".to_string(),
                reason: format!("Failed to build HTTP client: {}", e),
            })?;

        let active_model = std::sync::RwLock::new(config.model.clone());

        Ok(Self {
            client,
            config,
            active_model,
        })
    }

    /// Build the API URL based on the base URL.
    fn build_url(&self) -> String {
        // Use Anthropic-compatible endpoint for messages
        if self.config.base_url.contains("/apps/anthropic") {
            format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
        } else {
            // Fallback to OpenAI-compatible chat completions
            format!(
                "{}/chat/completions",
                self.config.base_url.trim_end_matches('/')
            )
        }
    }

    /// Build authorization header.
    fn build_auth_header(&self) -> Result<HeaderValue, LlmError> {
        let api_key = self
            .config
            .api_key
            .as_ref()
            .ok_or_else(|| LlmError::AuthFailed {
                provider: "aliyun".to_string(),
            })?;

        let value = format!("Bearer {}", api_key.expose_secret());
        HeaderValue::from_str(&value).map_err(|e| LlmError::RequestFailed {
            provider: "aliyun".to_string(),
            reason: format!("Failed to build auth header: {}", e),
        })
    }

    /// Convert IronClaw messages to API format.
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };

                if msg.content_parts.is_empty() {
                    serde_json::json!({
                        "role": role,
                        "content": msg.content
                    })
                } else {
                    // Handle multimodal content
                    let mut content_parts: Vec<serde_json::Value> = vec![serde_json::json!({
                        "type": "text",
                        "text": msg.content
                    })];

                    for part in &msg.content_parts {
                        match part {
                            crate::llm::provider::ContentPart::Text { text } => {
                                content_parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": text
                                }));
                            }
                            crate::llm::provider::ContentPart::ImageUrl { image_url } => {
                                content_parts.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": image_url.url.clone()
                                    }
                                }));
                            }
                        }
                    }

                    serde_json::json!({
                        "role": role,
                        "content": content_parts
                    })
                }
            })
            .collect()
    }

    /// Make a chat completion request.
    async fn complete_internal(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ToolDefinition>>,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<serde_json::Value, LlmError> {
        let url = self.build_url();
        let auth = self.build_auth_header()?;

        let model_name = model.as_deref().unwrap_or(&self.config.model);

        let mut body: serde_json::Value = serde_json::json!({
            "model": model_name,
            "messages": self.convert_messages(&messages),
            "max_tokens": max_tokens.unwrap_or(4096),
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Add tools if provided
        if let Some(tools) = tools {
            let tool_defs: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "tool_use",
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tool_defs);
        }

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(AUTHORIZATION, auth);

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::RequestFailed {
                provider: "aliyun".to_string(),
                reason: format!("HTTP request failed: {}", e),
            })?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(LlmError::RequestFailed {
                provider: "aliyun".to_string(),
                reason: format!("HTTP {}: {}", status.as_u16(), text),
            });
        }

        serde_json::from_str(&text).map_err(|e| LlmError::InvalidResponse {
            provider: "aliyun".to_string(),
            reason: format!("Failed to parse response: {}", e),
        })
    }

    /// Parse completion response.
    fn parse_completion_response(
        &self,
        response: serde_json::Value,
    ) -> Result<CompletionResponse, LlmError> {
        // Try Anthropic format first
        if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
            let mut text_content = String::new();
            let mut thinking_content = String::new();

            for item in content {
                if let Some(item_type) = item.get("type").and_then(|t| t.as_str()) {
                    match item_type {
                        "text" => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                text_content.push_str(text);
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = item.get("thinking").and_then(|t| t.as_str()) {
                                thinking_content.push_str(thinking);
                            }
                        }
                        _ => {}
                    }
                }
            }

            let usage = response.get("usage").or_else(|| response.get("usage"));
            let input_tokens = usage
                .and_then(|u| u.get("input_tokens").and_then(|i| i.as_u64()))
                .unwrap_or(0) as u32;
            let output_tokens = usage
                .and_then(|u| u.get("output_tokens").and_then(|o| o.as_u64()))
                .unwrap_or(0) as u32;
            let cache_read = usage
                .and_then(|u| u.get("cache_read_input_tokens").and_then(|c| c.as_u64()))
                .unwrap_or(0) as u32;
            let cache_creation = usage
                .and_then(|u| {
                    u.get("cache_creation_input_tokens")
                        .and_then(|c| c.as_u64())
                })
                .unwrap_or(0) as u32;

            let finish_reason = response
                .get("stop_reason")
                .and_then(|r| r.as_str())
                .map(|r| match r {
                    "end_turn" | "stop" => FinishReason::Stop,
                    "max_tokens" => FinishReason::Length,
                    "tool_use" => FinishReason::ToolUse,
                    _ => FinishReason::Unknown,
                })
                .unwrap_or(FinishReason::Unknown);

            return Ok(CompletionResponse {
                content: if thinking_content.is_empty() {
                    text_content
                } else {
                    format!("{}\n\n{}", thinking_content, text_content)
                },
                input_tokens,
                output_tokens,
                finish_reason,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: cache_creation,
            });
        }

        // Fallback to OpenAI format
        if let Some(choices) = response.get("choices").and_then(|c| c.as_array()) {
            if let Some(choice) = choices.first() {
                let content = choice
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();

                let usage = response.get("usage");
                let input_tokens = usage
                    .and_then(|u| u.get("prompt_tokens").and_then(|i| i.as_u64()))
                    .unwrap_or(0) as u32;
                let output_tokens = usage
                    .and_then(|u| u.get("completion_tokens").and_then(|o| o.as_u64()))
                    .unwrap_or(0) as u32;

                let finish_reason = choice
                    .get("finish_reason")
                    .and_then(|r| r.as_str())
                    .map(|r| match r {
                        "stop" => FinishReason::Stop,
                        "length" => FinishReason::Length,
                        _ => FinishReason::Unknown,
                    })
                    .unwrap_or(FinishReason::Unknown);

                return Ok(CompletionResponse {
                    content,
                    input_tokens,
                    output_tokens,
                    finish_reason,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                });
            }
        }

        Err(LlmError::InvalidResponse {
            provider: "aliyun".to_string(),
            reason: "Unexpected response format".to_string(),
        })
    }

    /// Parse tool completion response.
    fn parse_tool_response(
        &self,
        response: serde_json::Value,
    ) -> Result<ToolCompletionResponse, LlmError> {
        // Try Anthropic format first
        if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
            let mut text_content: Option<String> = None;
            let mut tool_calls = Vec::new();

            for item in content {
                if let Some(item_type) = item.get("type").and_then(|t| t.as_str()) {
                    match item_type {
                        "text" => {
                            text_content =
                                item.get("text").and_then(|t| t.as_str()).map(String::from);
                        }
                        "tool_use" => {
                            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                                    if let Some(input) = item.get("input") {
                                        tool_calls.push(ToolCall {
                                            id: id.to_string(),
                                            name: name.to_string(),
                                            arguments: input.clone(),
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            let usage = response.get("usage").or_else(|| response.get("usage"));
            let input_tokens = usage
                .and_then(|u| u.get("input_tokens").and_then(|i| i.as_u64()))
                .unwrap_or(0) as u32;
            let output_tokens = usage
                .and_then(|u| u.get("output_tokens").and_then(|o| o.as_u64()))
                .unwrap_or(0) as u32;
            let cache_read = usage
                .and_then(|u| u.get("cache_read_input_tokens").and_then(|c| c.as_u64()))
                .unwrap_or(0) as u32;
            let cache_creation = usage
                .and_then(|u| {
                    u.get("cache_creation_input_tokens")
                        .and_then(|c| c.as_u64())
                })
                .unwrap_or(0) as u32;

            let finish_reason = response
                .get("stop_reason")
                .and_then(|r| r.as_str())
                .map(|r| match r {
                    "end_turn" | "stop" => FinishReason::Stop,
                    "max_tokens" => FinishReason::Length,
                    "tool_use" => FinishReason::ToolUse,
                    _ => FinishReason::Unknown,
                })
                .unwrap_or(FinishReason::Unknown);

            return Ok(ToolCompletionResponse {
                content: text_content,
                tool_calls,
                input_tokens,
                output_tokens,
                finish_reason,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: cache_creation,
            });
        }

        // Fallback to OpenAI format
        if let Some(choices) = response.get("choices").and_then(|c| c.as_array()) {
            if let Some(choice) = choices.first() {
                let message = choice.get("message");

                let content = message
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(String::from);

                let tool_calls: Vec<ToolCall> = message
                    .and_then(|m| m.get("tool_calls"))
                    .and_then(|tc| tc.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|tc| {
                                let arguments_str = tc.get("function")?.get("arguments")?.as_str()?;
                                let arguments = serde_json::from_str(arguments_str).ok()?;
                                Some(ToolCall {
                                    id: tc.get("id")?.as_str()?.to_string(),
                                    name: tc.get("function")?.get("name")?.as_str()?.to_string(),
                                    arguments,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let usage = response.get("usage");
                let input_tokens = usage
                    .and_then(|u| u.get("prompt_tokens").and_then(|i| i.as_u64()))
                    .unwrap_or(0) as u32;
                let output_tokens = usage
                    .and_then(|u| u.get("completion_tokens").and_then(|o| o.as_u64()))
                    .unwrap_or(0) as u32;

                let finish_reason = choice
                    .get("finish_reason")
                    .and_then(|r| r.as_str())
                    .map(|r| match r {
                        "stop" => FinishReason::Stop,
                        "length" => FinishReason::Length,
                        "tool_calls" => FinishReason::ToolUse,
                        _ => FinishReason::Unknown,
                    })
                    .unwrap_or(FinishReason::Unknown);

                return Ok(ToolCompletionResponse {
                    content,
                    tool_calls,
                    input_tokens,
                    output_tokens,
                    finish_reason,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                });
            }
        }

        Err(LlmError::InvalidResponse {
            provider: "aliyun".to_string(),
            reason: "Unexpected response format".to_string(),
        })
    }
}

#[async_trait]
impl LlmProvider for AliyunProvider {
    fn model_name(&self) -> &str {
        &self.config.model
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        costs::model_cost(&self.config.model).unwrap_or_else(costs::default_cost)
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let response = self
            .complete_internal(
                request.messages,
                None,
                request.model,
                request.max_tokens,
                request.temperature,
            )
            .await?;

        self.parse_completion_response(response)
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let response = self
            .complete_internal(
                request.messages,
                Some(request.tools),
                request.model,
                request.max_tokens,
                request.temperature,
            )
            .await?;

        self.parse_tool_response(response)
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        // Try to fetch models from the API
        let url = format!("{}/v1/models", self.config.base_url.trim_end_matches('/'));

        if let Ok(auth) = self.build_auth_header() {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, auth);

            if let Ok(response) = self.client.get(&url).headers(headers).send().await {
                if let Ok(text) = response.text().await {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(models) = json.get("data").and_then(|d| d.as_array()) {
                            return Ok(models
                                .iter()
                                .filter_map(|m| {
                                    m.get("id")
                                        .or_else(|| m.get("model"))
                                        .and_then(|i| i.as_str())
                                        .map(String::from)
                                })
                                .collect());
                        }
                    }
                }
            }
        }

        // Return default models
        Ok(vec![
            "qwen3.5-plus".to_string(),
            "qwen3-max-2026-01-23".to_string(),
            "qwen3-coder-next".to_string(),
            "qwen3-coder-plus".to_string(),
            "MiniMax-M2.5".to_string(),
            "glm-5".to_string(),
            "glm-4.7".to_string(),
            "kimi-k2.5".to_string(),
        ])
    }
}
