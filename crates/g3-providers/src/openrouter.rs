//! OpenRouter provider implementation for the g3-providers crate.
//!
//! This module provides an implementation of the `LLMProvider` trait for OpenRouter's unified API,
//! which provides access to 200+ AI models through a single OpenAI-compatible endpoint.
//!
//! # Features
//!
//! - Support for 200+ models from multiple providers (Anthropic, OpenAI, Google, Meta, etc.)
//! - OpenAI-compatible API with provider routing extensions
//! - Both completion and streaming response modes
//! - Provider preference configuration for routing control
//! - Optional HTTP-Referer and X-Title headers for better analytics
//!
//! # Usage
//!
//! ```rust,no_run
//! use g3_providers::{OpenRouterProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Create the provider with your API key
//!     let provider = OpenRouterProvider::new(
//!         "your-api-key".to_string(),
//!         Some("anthropic/claude-3.5-sonnet".to_string()),
//!         None, // max_tokens
//!         None, // temperature
//!     )?;
//!
//!     // Create a completion request
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message::new(MessageRole::User, "Hello! How are you?".to_string()),
//!         ],
//!         max_tokens: Some(1000),
//!         temperature: Some(0.7),
//!         stream: false,
//!         tools: None,
//!         disable_thinking: false,
//!     };
//!
//!     // Get a completion
//!     let response = provider.complete(request).await?;
//!     println!("Response: {}", response.content);
//!
//!     Ok(())
//! }
//! ```

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error};

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_parameters: Option<bool>,
}

#[derive(Clone)]
pub struct OpenRouterProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: Option<u32>,
    _temperature: Option<f32>,
    name: String,
    provider_preferences: Option<ProviderPreferences>,
    http_referer: Option<String>,
    x_title: Option<String>,
}

impl OpenRouterProvider {
    pub fn new(
        api_key: String,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        Self::new_with_name(
            "openrouter".to_string(),
            api_key,
            model,
            max_tokens,
            temperature,
        )
    }

    pub fn new_with_name(
        name: String,
        api_key: String,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            api_key,
            model: model.unwrap_or_else(|| "anthropic/claude-3.5-sonnet".to_string()),
            base_url: OPENROUTER_BASE_URL.to_string(),
            max_tokens,
            _temperature: temperature,
            name,
            provider_preferences: None,
            http_referer: None,
            x_title: None,
        })
    }

    pub fn with_provider_preferences(mut self, preferences: ProviderPreferences) -> Self {
        self.provider_preferences = Some(preferences);
        self
    }

    pub fn with_http_referer(mut self, referer: String) -> Self {
        self.http_referer = Some(referer);
        self
    }

    pub fn with_x_title(mut self, title: String) -> Self {
        self.x_title = Some(title);
        self
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
        max_tokens: Option<u32>,
        _temperature: Option<f32>,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": self.model,
            "messages": convert_messages(messages),
            "stream": stream,
        });

        if let Some(max_tokens) = max_tokens.or(self.max_tokens) {
            body["max_tokens"] = json!(max_tokens);
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = json!(convert_tools(tools));
            }
        }

        if let Some(ref preferences) = self.provider_preferences {
            body["provider"] = serde_json::to_value(preferences).unwrap_or(json!({}));
        }

        if stream {
            body["stream_options"] = json!({
                "include_usage": true,
            });
        }

        body
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut accumulated_usage: Option<Usage> = None;
        let mut current_tool_calls: Vec<OpenRouterStreamingToolCall> = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let chunk_str = match std::str::from_utf8(&chunk) {
                        Ok(s) => s,
                        Err(e) => {
                            error!("Failed to parse chunk as UTF-8: {}", e);
                            continue;
                        }
                    };

                    buffer.push_str(chunk_str);

                    // Process complete lines
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer.drain(..line_end + 1);

                        if line.is_empty() {
                            continue;
                        }

                        // Parse Server-Sent Events format
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("Received stream completion marker");

                                let tool_calls = if current_tool_calls.is_empty() {
                                    None
                                } else {
                                    Some(
                                        current_tool_calls
                                            .iter()
                                            .filter_map(|tc| tc.to_tool_call())
                                            .collect(),
                                    )
                                };

                                let final_chunk = CompletionChunk {
                                    content: String::new(),
                                    finished: true,
                                    tool_calls,
                                    usage: accumulated_usage.clone(),
                                };
                                let _ = tx.send(Ok(final_chunk)).await;
                                return accumulated_usage;
                            }

                            // Parse the JSON data
                            match serde_json::from_str::<OpenRouterStreamChunk>(data) {
                                Ok(chunk_data) => {
                                    // Handle content
                                    for choice in &chunk_data.choices {
                                        if let Some(content) = &choice.delta.content {
                                            let chunk = CompletionChunk {
                                                content: content.clone(),
                                                finished: false,
                                                tool_calls: None,
                                                usage: None,
                                            };
                                            if tx.send(Ok(chunk)).await.is_err() {
                                                debug!("Receiver dropped, stopping stream");
                                                return accumulated_usage;
                                            }
                                        }

                                        // Handle tool calls
                                        if let Some(delta_tool_calls) = &choice.delta.tool_calls {
                                            for delta_tool_call in delta_tool_calls {
                                                if let Some(index) = delta_tool_call.index {
                                                    // Ensure we have enough tool calls in our vector
                                                    while current_tool_calls.len() <= index {
                                                        current_tool_calls.push(
                                                            OpenRouterStreamingToolCall::default(),
                                                        );
                                                    }

                                                    let tool_call = &mut current_tool_calls[index];

                                                    if let Some(id) = &delta_tool_call.id {
                                                        tool_call.id = Some(id.clone());
                                                    }

                                                    if let Some(function) =
                                                        &delta_tool_call.function
                                                    {
                                                        if let Some(name) = &function.name {
                                                            tool_call.name = Some(name.clone());
                                                        }
                                                        if let Some(arguments) = &function.arguments
                                                        {
                                                            tool_call.arguments.push_str(arguments);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Handle usage
                                    if let Some(usage) = chunk_data.usage {
                                        accumulated_usage = Some(Usage {
                                            prompt_tokens: usage.prompt_tokens,
                                            completion_tokens: usage.completion_tokens,
                                            total_tokens: usage.total_tokens,
                                        });
                                    }
                                }
                                Err(e) => {
                                    debug!("Failed to parse stream chunk: {} - Data: {}", e, data);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let _ = tx.send(Err(anyhow::anyhow!("Stream error: {}", e))).await;
                    return accumulated_usage;
                }
            }
        }

        // Send final chunk if we haven't already
        let tool_calls = if current_tool_calls.is_empty() {
            None
        } else {
            Some(
                current_tool_calls
                    .iter()
                    .filter_map(|tc| tc.to_tool_call())
                    .collect(),
            )
        };

        let final_chunk = CompletionChunk {
            content: String::new(),
            finished: true,
            tool_calls,
            usage: accumulated_usage.clone(),
        };
        let _ = tx.send(Ok(final_chunk)).await;

        accumulated_usage
    }
}

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing OpenRouter completion request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            request.max_tokens,
            request.temperature,
        );

        debug!("Sending request to OpenRouter API: model={}", self.model);

        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key));

        if let Some(ref referer) = self.http_referer {
            req = req.header("HTTP-Referer", referer);
        }

        if let Some(ref title) = self.x_title {
            req = req.header("X-Title", title);
        }

        let response = req.json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "OpenRouter API error {}: {}",
                status,
                error_text
            ));
        }

        let openrouter_response: OpenRouterResponse = response.json().await?;

        let content = openrouter_response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_default();

        let usage = Usage {
            prompt_tokens: openrouter_response.usage.prompt_tokens,
            completion_tokens: openrouter_response.usage.completion_tokens,
            total_tokens: openrouter_response.usage.total_tokens,
        };

        debug!(
            "OpenRouter completion successful: {} tokens generated",
            usage.completion_tokens
        );

        Ok(CompletionResponse {
            content,
            usage,
            model: self.model.clone(),
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        debug!(
            "Processing OpenRouter streaming request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
            request.max_tokens,
            request.temperature,
        );

        debug!(
            "Sending streaming request to OpenRouter API: model={}",
            self.model
        );

        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key));

        if let Some(ref referer) = self.http_referer {
            req = req.header("HTTP-Referer", referer);
        }

        if let Some(ref title) = self.x_title {
            req = req.header("X-Title", title);
        }

        let response = req.json(&body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "OpenRouter API error {}: {}",
                status,
                error_text
            ));
        }

        let stream = response.bytes_stream();
        let (tx, rx) = mpsc::channel(100);

        // Spawn task to process the stream
        let provider = self.clone();
        tokio::spawn(async move {
            let usage = provider.parse_streaming_response(stream, tx).await;
            // Log the final usage if available
            if let Some(usage) = usage {
                debug!(
                    "Stream completed with usage - prompt: {}, completion: {}, total: {}",
                    usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                );
            }
        });

        Ok(ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // OpenRouter supports tool calling via OpenAI-compatible format
        true
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens.unwrap_or(4096)
    }

    fn temperature(&self) -> f32 {
        self._temperature.unwrap_or(0.7)
    }
}

fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|msg| {
            json!({
                "role": match msg.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                },
                "content": msg.content,
            })
        })
        .collect()
}

fn convert_tools(tools: &[Tool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect()
}

// OpenRouter API response structures (OpenAI-compatible)
#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    choices: Vec<OpenRouterChoice>,
    usage: OpenRouterUsage,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
    message: OpenRouterMessage,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenRouterMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenRouterToolCall>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenRouterToolCall {
    id: String,
    function: OpenRouterFunction,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct OpenRouterFunction {
    name: String,
    arguments: String,
}

// Streaming tool call accumulator
#[derive(Debug, Default)]
struct OpenRouterStreamingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl OpenRouterStreamingToolCall {
    fn to_tool_call(&self) -> Option<ToolCall> {
        let id = self.id.as_ref()?;
        let name = self.name.as_ref()?;

        let args = serde_json::from_str(&self.arguments).unwrap_or(serde_json::Value::Null);

        Some(ToolCall {
            id: id.clone(),
            tool: name.clone(),
            args,
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// Streaming response structures
#[derive(Debug, Deserialize)]
struct OpenRouterStreamChunk {
    choices: Vec<OpenRouterStreamChoice>,
    usage: Option<OpenRouterUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterStreamChoice {
    delta: OpenRouterDelta,
}

#[derive(Debug, Deserialize)]
struct OpenRouterDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenRouterDeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterDeltaToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OpenRouterDeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterDeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}
