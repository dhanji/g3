//! Ollama LLM provider implementation for the g3-providers crate.
//!
//! This module provides an implementation of the `LLMProvider` trait for Ollama,
//! supporting both completion and streaming modes with native tool calling.
//!
//! # Features
//!
//! - Support for any Ollama model (llama3.2, mistral, qwen, etc.)
//! - Both completion and streaming response modes
//! - Native tool calling support for compatible models
//! - Configurable base URL (defaults to http://localhost:11434)
//! - Simple configuration with no authentication required
//!
//! # Usage
//!
//! ```rust,no_run
//! use g3_providers::{OllamaProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Create the provider with default settings (localhost:11434)
//!     let provider = OllamaProvider::new(
//!         "llama3.2".to_string(),
//!         None, // Optional: base_url
//!         None, // Optional: max tokens
//!         None, // Optional: temperature
//!     )?;
//!
//!     // Create a completion request
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message {
//!                 role: MessageRole::User,
//!                 content: "Hello! How are you?".to_string(),
//!             },
//!         ],
//!         max_tokens: Some(1000),
//!         temperature: Some(0.7),
//!         stream: false,
//!         tools: None,
//!     };
//!
//!     // Get a completion
//!     let response = provider.complete(request).await?;
//!     println!("Response: {}", response.content);
//!
//!     Ok(())
//! }
//! ```

use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures_util::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_TIMEOUT_SECS: u64 = 600;

pub const OLLAMA_DEFAULT_MODEL: &str = "llama3.2";
pub const OLLAMA_KNOWN_MODELS: &[&str] = &[
    "llama3.2",
    "llama3.2:1b",
    "llama3.2:3b",
    "llama3.1",
    "llama3.1:8b",
    "llama3.1:70b",
    "mistral",
    "mistral-nemo",
    "mixtral",
    "qwen2.5",
    "qwen2.5:7b",
    "qwen2.5:14b",
    "qwen2.5:32b",
    "phi3",
    "gemma2",
];

#[derive(Debug, Clone)]
pub struct OllamaProvider {
    client: Client,
    base_url: String,
    model: String,
    max_tokens: Option<u32>,
    temperature: f32,
}

impl OllamaProvider {
    pub fn new(
        model: String,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let base_url = base_url
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();

        info!(
            "Initialized Ollama provider with model: {} at {}",
            model, base_url
        );

        Ok(Self {
            client,
            base_url,
            model,
            max_tokens,
            temperature: temperature.unwrap_or(0.7),
        })
    }

    fn convert_tools(&self, tools: &[Tool]) -> Vec<OllamaTool> {
        tools
            .iter()
            .map(|tool| OllamaTool {
                r#type: "function".to_string(),
                function: OllamaFunction {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                },
            })
            .collect()
    }

    fn convert_messages(&self, messages: &[Message]) -> Result<Vec<OllamaMessage>> {
        let mut ollama_messages = Vec::new();

        for message in messages {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            };

            ollama_messages.push(OllamaMessage {
                role: role.to_string(),
                content: message.content.clone(),
                tool_calls: None, // Only used in responses
            });
        }

        if ollama_messages.is_empty() {
            return Err(anyhow!("At least one message is required"));
        }

        Ok(ollama_messages)
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        streaming: bool,
        max_tokens: Option<u32>,
        temperature: f32,
    ) -> Result<OllamaRequest> {
        let ollama_messages = self.convert_messages(messages)?;
        let ollama_tools = tools.map(|t| self.convert_tools(t));

        let mut options = OllamaOptions {
            temperature,
            num_predict: max_tokens,
        };

        // If max_tokens is provided, use it; otherwise use the instance default
        if max_tokens.is_none() {
            options.num_predict = self.max_tokens;
        }

        let request = OllamaRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            tools: ollama_tools,
            stream: streaming,
            options,
        };

        Ok(request)
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut accumulated_usage: Option<Usage> = None;
        let mut current_tool_calls: Vec<OllamaToolCall> = Vec::new();
        let mut byte_buffer = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    // Append new bytes to our buffer
                    byte_buffer.extend_from_slice(&chunk);

                    // Try to convert the entire buffer to UTF-8
                    let chunk_str = match std::str::from_utf8(&byte_buffer) {
                        Ok(s) => {
                            let result = s.to_string();
                            byte_buffer.clear();
                            result
                        }
                        Err(e) => {
                            let valid_up_to = e.valid_up_to();
                            if valid_up_to > 0 {
                                let valid_bytes =
                                    byte_buffer.drain(..valid_up_to).collect::<Vec<_>>();
                                std::str::from_utf8(&valid_bytes).unwrap().to_string()
                            } else {
                                continue;
                            }
                        }
                    };

                    buffer.push_str(&chunk_str);

                    // Process complete lines
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer.drain(..line_end + 1);

                        if line.is_empty() {
                            continue;
                        }

                        // Ollama streaming sends JSON objects per line
                        match serde_json::from_str::<OllamaStreamChunk>(&line) {
                            Ok(chunk) => {
                                // Handle text content
                                if let Some(message) = &chunk.message {
                                    let content = &message.content;
                                    if !content.is_empty() {
                                        debug!("Sending text chunk: '{}'", content);
                                        let chunk = CompletionChunk {
                                            content: content.clone(),
                                            finished: false,
                                            usage: None,
                                            tool_calls: None,
                                        };
                                        if tx.send(Ok(chunk)).await.is_err() {
                                            debug!("Receiver dropped, stopping stream");
                                            return accumulated_usage;
                                        }
                                    }

                                    // Handle tool calls
                                    if let Some(tool_calls) = &message.tool_calls {
                                        current_tool_calls.extend(tool_calls.clone());
                                    }
                                }

                                // Check if stream is done
                                if chunk.done.unwrap_or(false) {
                                    debug!("Stream completed");

                                    // Update usage if available
                                    if let Some(eval_count) = chunk.eval_count {
                                        accumulated_usage = Some(Usage {
                                            prompt_tokens: chunk.prompt_eval_count.unwrap_or(0),
                                            completion_tokens: eval_count,
                                            total_tokens: chunk.prompt_eval_count.unwrap_or(0)
                                                + eval_count,
                                        });
                                    }

                                    // Send final chunk with tool calls if any
                                    let final_tool_calls: Vec<ToolCall> = current_tool_calls
                                        .iter()
                                        .map(|tc| ToolCall {
                                            id: tc.function.name.clone(), // Ollama doesn't provide IDs
                                            tool: tc.function.name.clone(),
                                            args: tc.function.arguments.clone(),
                                        })
                                        .collect();

                                    let final_chunk = CompletionChunk {
                                        content: String::new(),
                                        finished: true,
                                        usage: accumulated_usage.clone(),
                                        tool_calls: if final_tool_calls.is_empty() {
                                            None
                                        } else {
                                            Some(final_tool_calls)
                                        },
                                    };
                                    if tx.send(Ok(final_chunk)).await.is_err() {
                                        debug!("Receiver dropped, stopping stream");
                                    }
                                    return accumulated_usage;
                                }
                            }
                            Err(e) => {
                                debug!("Failed to parse Ollama stream chunk: {} - Line: {}", e, line);
                                // Don't error out, just continue
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let error_msg = e.to_string();
                    if error_msg.contains("unexpected EOF") || error_msg.contains("connection") {
                        warn!("Connection terminated unexpectedly, treating as end of stream");
                        break;
                    } else {
                        let _ = tx.send(Err(anyhow!("Stream error: {}", e))).await;
                    }
                    return accumulated_usage;
                }
            }
        }

        // Send final chunk if we haven't already
        let final_tool_calls: Vec<ToolCall> = current_tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.function.name.clone(),
                tool: tc.function.name.clone(),
                args: tc.function.arguments.clone(),
            })
            .collect();

        let final_chunk = CompletionChunk {
            content: String::new(),
            finished: true,
            usage: accumulated_usage.clone(),
            tool_calls: if final_tool_calls.is_empty() {
                None
            } else {
                Some(final_tool_calls)
            },
        };
        let _ = tx.send(Ok(final_chunk)).await;
        accumulated_usage
    }

    /// Fetch available models from the Ollama instance
    pub async fn fetch_available_models(&self) -> Result<Vec<String>> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch Ollama models: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!(
                "Failed to fetch Ollama models: {} - {}",
                status,
                error_text
            ));
        }

        let json: serde_json::Value = response.json().await?;
        let models = json
            .get("models")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("Unexpected response format: missing 'models' array"))?;

        let model_names: Vec<String> = models
            .iter()
            .filter_map(|model| model.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();

        debug!("Found {} models in Ollama", model_names.len());
        Ok(model_names)
    }
}

#[async_trait::async_trait]
impl LLMProvider for OllamaProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing Ollama completion request with {} messages",
            request.messages.len()
        );

        let max_tokens = request.max_tokens.or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            max_tokens,
            temperature,
        )?;

        debug!(
            "Sending request to Ollama API: model={}, temperature={}",
            self.model, request_body.options.temperature
        );

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Ollama API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!("Ollama API error {}: {}", status, error_text));
        }

        let response_text = response.text().await?;
        debug!("Raw Ollama API response: {}", response_text);

        let ollama_response: OllamaResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                anyhow!(
                    "Failed to parse Ollama response: {} - Response: {}",
                    e,
                    response_text
                )
            })?;

        let content = ollama_response.message.content.clone();

        let usage = Usage {
            prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
            completion_tokens: ollama_response.eval_count.unwrap_or(0),
            total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                + ollama_response.eval_count.unwrap_or(0),
        };

        debug!(
            "Ollama completion successful: {} tokens generated",
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
            "Processing Ollama request (non-streaming) with {} messages",
            request.messages.len()
        );

        if let Some(ref tools) = request.tools {
            debug!("Request has {} tools", tools.len());
            for tool in tools.iter().take(5) {
                debug!("  Tool: {}", tool.name);
            }
        }

        let max_tokens = request.max_tokens.or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false, // Use non-streaming mode to avoid streaming bugs
            max_tokens,
            temperature,
        )?;

        debug!(
            "Sending request to Ollama API (stream=false): model={}, temperature={}",
            self.model, request_body.options.temperature
        );

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Ollama API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!("Ollama API error {}: {}", status, error_text));
        }

        // For non-streaming, parse the complete JSON response
        let response_text = response.text().await?;
        debug!("Raw Ollama API response: {}", response_text);

        let ollama_response: OllamaResponse =
            serde_json::from_str(&response_text).map_err(|e| {
                anyhow!(
                    "Failed to parse Ollama response: {} - Response: {}",
                    e,
                    response_text
                )
            })?;

        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(async move {
            let content = ollama_response.message.content;
            let usage = Usage {
                prompt_tokens: ollama_response.prompt_eval_count.unwrap_or(0),
                completion_tokens: ollama_response.eval_count.unwrap_or(0),
                total_tokens: ollama_response.prompt_eval_count.unwrap_or(0)
                    + ollama_response.eval_count.unwrap_or(0),
            };

            // Extract tool calls if present
            let tool_calls: Option<Vec<ToolCall>> = ollama_response.message.tool_calls.map(|tcs| {
                tcs.iter()
                    .map(|tc| ToolCall {
                        id: tc.function.name.clone(),
                        tool: tc.function.name.clone(),
                        args: tc.function.arguments.clone(),
                    })
                    .collect()
            });

            // Send content if any
            if !content.is_empty() {
                let _ = tx.send(Ok(CompletionChunk {
                    content,
                    finished: false,
                    usage: None,
                    tool_calls: None,
                })).await;
            }

            // Send final chunk with usage and tool calls
            let _ = tx.send(Ok(CompletionChunk {
                content: String::new(),
                finished: true,
                usage: Some(usage),
                tool_calls,
            })).await;
        });

        Ok(ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        "ollama"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // Most modern Ollama models support tool calling
        // Models like llama3.2, qwen2.5, mistral, etc. have good tool support
        true
    }
}

// Ollama API request/response structures

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaTool>>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>, // Ollama's equivalent of max_tokens
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    r#type: String,
    function: OllamaFunction,
}

#[derive(Debug, Serialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    #[allow(dead_code)]
    done: bool,
    #[allow(dead_code)]
    total_duration: Option<u64>,
    #[allow(dead_code)]
    load_duration: Option<u64>,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OllamaStreamChunk {
    message: Option<OllamaMessage>,
    done: Option<bool>,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_creation() {
        let provider = OllamaProvider::new(
            "llama3.2".to_string(),
            None,
            Some(1000),
            Some(0.7),
        )
        .unwrap();

        assert_eq!(provider.model(), "llama3.2");
        assert_eq!(provider.name(), "ollama");
        assert!(provider.has_native_tool_calling());
    }

    #[test]
    fn test_message_conversion() {
        let provider = OllamaProvider::new(
            "llama3.2".to_string(),
            None,
            None,
            None,
        )
        .unwrap();

        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are a helpful assistant.".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello!".to_string(),
            },
        ];

        let ollama_messages = provider.convert_messages(&messages).unwrap();

        assert_eq!(ollama_messages.len(), 2);
        assert_eq!(ollama_messages[0].role, "system");
        assert_eq!(ollama_messages[1].role, "user");
    }

    #[test]
    fn test_tool_conversion() {
        let provider = OllamaProvider::new(
            "llama3.2".to_string(),
            None,
            None,
            None,
        )
        .unwrap();

        let tools = vec![Tool {
            name: "get_weather".to_string(),
            description: "Get the current weather".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "The city and state"
                    }
                },
                "required": ["location"]
            }),
        }];

        let ollama_tools = provider.convert_tools(&tools);

        assert_eq!(ollama_tools.len(), 1);
        assert_eq!(ollama_tools[0].r#type, "function");
        assert_eq!(ollama_tools[0].function.name, "get_weather");
    }

    #[test]
    fn test_custom_base_url() {
        let provider = OllamaProvider::new(
            "llama3.2".to_string(),
            Some("http://custom:11434".to_string()),
            None,
            None,
        )
        .unwrap();

        assert_eq!(provider.base_url, "http://custom:11434");
    }
}
