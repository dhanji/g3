//! Anthropic Claude provider implementation for the g3-providers crate.
//!
//! This module provides an implementation of the `LLMProvider` trait for Anthropic's Claude models,
//! supporting both completion and streaming modes through the Anthropic Messages API.
//!
//! # Features
//!
//! - Support for all Claude models (claude-3-5-sonnet-20241022, claude-3-haiku-20240307, etc.)
//! - Both completion and streaming response modes
//! - Proper message format conversion between g3 and Anthropic formats
//! - Rate limiting and error handling
//! - Native tool calling support
//! - Custom base URL support for proxies or alternative API endpoints
//!
//! # Usage
//!
//! ```rust,no_run
//! use g3_providers::{AnthropicProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Create the provider with your API key
//!     let provider = AnthropicProvider::new(
//!         "your-api-key".to_string(),
//!         Some("claude-3-5-sonnet-20241022".to_string()),
//!         None, // base_url (uses default Anthropic API)
//!         Some(4096),
//!         Some(0.1),
//!         None, // cache_config
//!         None, // enable_1m_context
//!         None, // thinking_budget_tokens
//!     )?;
//!
//!     // Create a completion request
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message::new(MessageRole::System, "You are a helpful assistant.".to_string()),
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
//!
//! # Streaming Example
//!
//! ```rust,no_run
//! use g3_providers::{AnthropicProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let provider = AnthropicProvider::new(
//!         "your-api-key".to_string(),
//!         None, // model
//!         None, // base_url
//!         None, // max_tokens
//!         None, // temperature
//!         None, // cache_config
//!         None, // enable_1m_context
//!         None, // thinking_budget_tokens
//!     )?;
//!
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message::new(MessageRole::User, "Write a short story about a robot.".to_string()),
//!         ],
//!         max_tokens: Some(1000),
//!         temperature: Some(0.7),
//!         stream: true,
//!         tools: None,
//!         disable_thinking: false,
//!     };
//!
//!     let mut stream = provider.stream(request).await?;
//!     while let Some(chunk) = stream.next().await {
//!         match chunk {
//!             Ok(chunk) => {
//!                 print!("{}", chunk.content);
//!                 if chunk.finished {
//!                     break;
//!                 }
//!             }
//!             Err(e) => {
//!                 eprintln!("Stream error: {}", e);
//!                 break;
//!             }
//!         }
//!     }
//!
//!     Ok(())
//! }
//! ```

use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures_util::stream::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error};

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    client: Client,
    name: String,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
    temperature: f32,
    #[allow(dead_code)]
    cache_config: Option<String>,
    enable_1m_context: bool,
    thinking_budget_tokens: Option<u32>,
}

impl AnthropicProvider {
    pub fn new(
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        cache_config: Option<String>,
        enable_1m_context: Option<bool>,
        thinking_budget_tokens: Option<u32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let model = model.unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string());
        let base_url = base_url.unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string());

        debug!("Initialized Anthropic provider with model: {}, base_url: {}", model, base_url);

        Ok(Self {
            client,
            name: "anthropic".to_string(),
            api_key,
            model,
            base_url,
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.1),
            cache_config,
            enable_1m_context: enable_1m_context.unwrap_or(false),
            thinking_budget_tokens,
        })
    }

    /// Create a new AnthropicProvider with a custom name (e.g., "anthropic.default")
    pub fn new_with_name(
        name: String,
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        cache_config: Option<String>,
        enable_1m_context: Option<bool>,
        thinking_budget_tokens: Option<u32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let model = model.unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string());
        let base_url = base_url.unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string());

        debug!("Initialized Anthropic provider '{}' with model: {}, base_url: {}", name, model, base_url);

        Ok(Self {
            client,
            name,
            api_key,
            model,
            base_url,
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.1),
            cache_config,
            enable_1m_context: enable_1m_context.unwrap_or(false),
            thinking_budget_tokens,
        })
    }

    fn create_request_builder(&self, streaming: bool) -> RequestBuilder {
        let url = format!("{}/messages", self.base_url);
        let mut builder = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");

        if self.enable_1m_context {
            builder = builder.header("anthropic-beta", "context-1m-2025-08-07");
        }

        if streaming {
            builder = builder.header("accept", "text/event-stream");
        }

        builder
    }

    fn convert_cache_control(cache_control: &crate::CacheControl) -> crate::CacheControl {
        // Anthropic uses the same format, so just clone it
        cache_control.clone()
    }

    fn convert_tools(&self, tools: &[Tool]) -> Vec<AnthropicTool> {
        tools
            .iter()
            .map(|tool| {
                let mut schema = AnthropicToolInputSchema {
                    schema_type: "object".to_string(),
                    properties: serde_json::Value::Object(serde_json::Map::new()),
                    required: None,
                };

                // Extract properties and required fields from the input schema
                if let Ok(schema_obj) = serde_json::from_value::<
                    serde_json::Map<String, serde_json::Value>,
                >(tool.input_schema.clone())
                {
                    if let Some(properties) = schema_obj.get("properties") {
                        schema.properties = properties.clone();
                    }
                    if let Some(required) = schema_obj.get("required") {
                        if let Ok(required_vec) =
                            serde_json::from_value::<Vec<String>>(required.clone())
                        {
                            schema.required = Some(required_vec);
                        }
                    }
                }

                AnthropicTool {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: schema,
                }
            })
            .collect()
    }

    fn convert_messages(
        &self,
        messages: &[Message],
    ) -> Result<(Option<String>, Vec<AnthropicMessage>)> {
        let mut system_message = None;
        let mut anthropic_messages = Vec::new();

        for message in messages {
            match message.role {
                MessageRole::System => {
                    if let Some(existing) = system_message {
                        // Concatenate system messages instead of replacing
                        system_message = Some(format!("{}\n\n{}", existing, message.content));
                    } else {
                        system_message = Some(message.content.clone());
                    }
                }
                MessageRole::User => {
                    // Build content blocks - images first, then text
                    let mut content_blocks: Vec<AnthropicContent> = Vec::new();
                    
                    // Add any images attached to this message
                    for image in &message.images {
                        content_blocks.push(AnthropicContent::Image {
                            source: AnthropicImageSource {
                                source_type: "base64".to_string(),
                                media_type: image.media_type.clone(),
                                data: image.data.clone(),
                            },
                        });
                    }
                    
                    // Add text content
                    content_blocks.push(AnthropicContent::Text {
                        text: message.content.clone(),
                        cache_control: message.cache_control.as_ref().map(Self::convert_cache_control),
                    });
                    
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: content_blocks,
                    });
                }
                MessageRole::Assistant => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: vec![AnthropicContent::Text {
                            text: message.content.clone(),
                            cache_control: message
                                .cache_control
                                .as_ref()
                                .map(Self::convert_cache_control),
                        }],
                    });
                }
            }
        }

        Ok((system_message, anthropic_messages))
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        streaming: bool,
        max_tokens: u32,
        temperature: f32,
        disable_thinking: bool,
    ) -> Result<AnthropicRequest> {
        let (system, anthropic_messages) = self.convert_messages(messages)?;

        if anthropic_messages.is_empty() {
            return Err(anyhow!(
                "At least one user or assistant message is required"
            ));
        }

        // Convert tools if provided
        let anthropic_tools = tools.map(|t| self.convert_tools(t));

        // Add thinking configuration if budget_tokens is set AND max_tokens is sufficient AND not explicitly disabled
        // Anthropic requires: max_tokens > thinking.budget_tokens
        // We add 1024 as minimum buffer for actual response content
        tracing::debug!("create_request_body called: max_tokens={}, disable_thinking={}, thinking_budget_tokens={:?}", max_tokens, disable_thinking, self.thinking_budget_tokens);

        let thinking = if disable_thinking {
            tracing::debug!(
                "Thinking mode explicitly disabled for this request (max_tokens={})",
                max_tokens
            );
            None
        } else {
            self.thinking_budget_tokens.and_then(|budget| {
            let min_required = budget + 1024;
            if max_tokens > min_required {
                Some(ThinkingConfig::enabled(budget))
            } else {
                tracing::warn!(
                    "Disabling thinking mode: max_tokens ({}) is not greater than thinking.budget_tokens ({}) + 1024 buffer. \
                     Required: max_tokens > {}",
                    max_tokens, budget, min_required
                );
                None
            }
            })
        };

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            temperature,
            messages: anthropic_messages,
            system,
            tools: anthropic_tools,
            stream: streaming,
            thinking,
        };

        // Ensure the conversation starts with a user message
        if request.messages[0].role != "user" {
            return Err(anyhow!("Conversation must start with a user message"));
        }

        Ok(request)
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut current_tool_calls: Vec<ToolCall> = Vec::new();
        let mut partial_tool_json = String::new(); // Accumulate partial JSON for tool calls
        let mut accumulated_usage: Option<Usage> = None;
        let mut byte_buffer = Vec::new(); // Buffer for incomplete UTF-8 sequences
        let mut message_stopped = false; // Track if we've received message_stop

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    // Append new bytes to our buffer
                    byte_buffer.extend_from_slice(&chunk);

                    // Try to convert the entire buffer to UTF-8
                    let chunk_str = match std::str::from_utf8(&byte_buffer) {
                        Ok(s) => {
                            // Successfully converted entire buffer, clear it and use the string
                            let result = s.to_string();
                            byte_buffer.clear();
                            result
                        }
                        Err(e) => {
                            // Check if this is an incomplete sequence at the end
                            let valid_up_to = e.valid_up_to();
                            if valid_up_to > 0 {
                                // We have some valid UTF-8, extract it and keep the rest for next iteration
                                let valid_bytes =
                                    byte_buffer.drain(..valid_up_to).collect::<Vec<_>>();
                                std::str::from_utf8(&valid_bytes).unwrap().to_string()
                            } else {
                                // No valid UTF-8 at all, skip this chunk and continue
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

                        // If we've already sent the final chunk, skip processing more events
                        if message_stopped {
                            debug!("Skipping event after message_stop: {}", line);
                            continue;
                        }

                        // Parse Server-Sent Events format
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("Received stream completion marker");
                                let final_chunk = CompletionChunk {
                                    content: String::new(),
                                    finished: true,
                                    usage: accumulated_usage.clone(),
                                    tool_calls: if current_tool_calls.is_empty() {
                                        None
                                    } else {
                                        Some(current_tool_calls.clone())
                                    },
                                };
                                if tx.send(Ok(final_chunk)).await.is_err() {
                                    debug!("Receiver dropped, stopping stream");
                                }
                                return accumulated_usage;
                            }

                            debug!("Raw Claude API JSON: {}", data);

                            match serde_json::from_str::<AnthropicStreamEvent>(data) {
                                Ok(event) => {
                                    debug!(
                                        "Parsed event type: {}, event: {:?}",
                                        event.event_type, event
                                    );
                                    match event.event_type.as_str() {
                                        "message_start" => {
                                            // Extract usage data from message_start event
                                            if let Some(message) = event.message {
                                                if let Some(usage) = message.usage {
                                                    accumulated_usage = Some(Usage {
                                                        prompt_tokens: usage.input_tokens,
                                                        completion_tokens: usage.output_tokens,
                                                        total_tokens: usage.input_tokens
                                                            + usage.output_tokens,
                                                    });
                                                    debug!(
                                                        "Captured usage from message_start: {:?}",
                                                        accumulated_usage
                                                    );
                                                }
                                            }
                                        }
                                        "content_block_start" => {
                                            debug!(
                                                "Received content_block_start event: {:?}",
                                                event
                                            );
                                            if let Some(content_block) = event.content_block {
                                                match content_block {
                                                    AnthropicContent::ToolUse {
                                                        id,
                                                        name,
                                                        input,
                                                    } => {
                                                        debug!("Found tool use in content_block_start: id={}, name={}, input={:?}", id, name, input);

                                                        // For native tool calls, create the tool call immediately if we have complete args
                                                        // If args are empty, we'll wait for partial_json to accumulate them
                                                        let tool_call = ToolCall {
                                                            id: id.clone(),
                                                            tool: name.clone(),
                                                            args: input.clone(),
                                                        };

                                                        // Check if we already have complete arguments
                                                        if !input.is_null()
                                                            && input
                                                                != serde_json::Value::Object(
                                                                    serde_json::Map::new(),
                                                                )
                                                        {
                                                            // We have complete arguments, send the tool call immediately
                                                            debug!("Tool call has complete args, sending immediately: {:?}", tool_call);
                                                            let chunk = CompletionChunk {
                                                                content: String::new(),
                                                                finished: false,
                                                                usage: None,
                                                                tool_calls: Some(vec![tool_call]),
                                                            };
                                                            if tx.send(Ok(chunk)).await.is_err() {
                                                                debug!("Receiver dropped, stopping stream");
                                                                return accumulated_usage;
                                                            }
                                                        } else {
                                                            // Arguments are empty, we'll accumulate them from partial_json
                                                            debug!("Tool call has empty args, will accumulate from partial_json");
                                                            current_tool_calls.push(tool_call);
                                                            partial_tool_json.clear();
                                                        }
                                                    }
                                                    _ => {
                                                        debug!(
                                                            "Non-tool content block: {:?}",
                                                            content_block
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        "content_block_delta" => {
                                            if let Some(delta) = event.delta {
                                                if let Some(text) = delta.text {
                                                    debug!(
                                                        "Sending text chunk of length {}: '{}'",
                                                        text.len(),
                                                        text
                                                    );
                                                    let chunk = CompletionChunk {
                                                        content: text,
                                                        finished: false,
                                                        usage: None,
                                                        tool_calls: None,
                                                    };
                                                    if tx.send(Ok(chunk)).await.is_err() {
                                                        debug!("Receiver dropped, stopping stream");
                                                        return accumulated_usage;
                                                    }
                                                }
                                                // Handle partial JSON for tool calls
                                                if let Some(partial_json) = delta.partial_json {
                                                    debug!(
                                                        "Received partial JSON: {}",
                                                        partial_json
                                                    );
                                                    partial_tool_json.push_str(&partial_json);
                                                    debug!(
                                                        "Accumulated tool JSON: {}",
                                                        partial_tool_json
                                                    );
                                                }
                                            }
                                        }
                                        "content_block_stop" => {
                                            // Tool call block is complete - now parse the accumulated JSON
                                            if !current_tool_calls.is_empty()
                                                && !partial_tool_json.is_empty()
                                            {
                                                debug!(
                                                    "Parsing complete tool JSON: {}",
                                                    partial_tool_json
                                                );

                                                // Parse the accumulated JSON and update the last tool call
                                                if let Ok(parsed_args) =
                                                    serde_json::from_str::<serde_json::Value>(
                                                        &partial_tool_json,
                                                    )
                                                {
                                                    if let Some(last_tool) =
                                                        current_tool_calls.last_mut()
                                                    {
                                                        last_tool.args = parsed_args;
                                                        debug!("Updated tool call with complete args: {:?}", last_tool);
                                                    }
                                                } else {
                                                    debug!(
                                                        "Failed to parse accumulated JSON: {}",
                                                        partial_tool_json
                                                    );
                                                }

                                                // Clear the accumulator
                                                partial_tool_json.clear();
                                            }

                                            // Send the complete tool call
                                            if !current_tool_calls.is_empty() {
                                                let chunk = CompletionChunk {
                                                    content: String::new(),
                                                    finished: false,
                                                    usage: None,
                                                    tool_calls: Some(current_tool_calls.clone()),
                                                };
                                                if tx.send(Ok(chunk)).await.is_err() {
                                                    debug!("Receiver dropped, stopping stream");
                                                    return accumulated_usage;
                                                }
                                                // Clear tool calls after sending to prevent duplicates at message_stop
                                                current_tool_calls.clear();
                                            }
                                        }
                                        "message_stop" => {
                                            debug!("Received message stop event");
                                            message_stopped = true;
                                            let final_chunk = CompletionChunk {
                                                content: String::new(),
                                                finished: true,
                                                usage: accumulated_usage.clone(),
                                                tool_calls: if current_tool_calls.is_empty() {
                                                    None
                                                } else {
                                                    Some(current_tool_calls.clone())
                                                },
                                            };
                                            if tx.send(Ok(final_chunk)).await.is_err() {
                                                debug!("Receiver dropped, stopping stream");
                                            }
                                            // Don't return here - let the stream naturally exhaust
                                            // This prevents dropping the sender prematurely
                                        }
                                        "error" => {
                                            if let Some(error) = event.error {
                                                error!("Anthropic API error: {:?}", error);
                                                let _ = tx
                                                    .send(Err(anyhow!(
                                                        "Anthropic API error: {:?}",
                                                        error
                                                    )))
                                                    .await;
                                                break; // Break to let stream exhaust naturally
                                            }
                                        }
                                        _ => {
                                            debug!("Ignoring event type: {}", event.event_type);
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!("Failed to parse stream event: {} - Data: {}", e, data);
                                    // Don't error out on parse failures, just continue
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let _ = tx.send(Err(anyhow!("Stream error: {}", e))).await;
                    // Don't return here either - let the stream exhaust naturally
                    // The error has been sent to the receiver, so it will handle it
                    // Breaking here ensures we clean up properly
                    break;
                }
            }
        }

        // Send final chunk if we haven't already
        let final_chunk = CompletionChunk {
            content: String::new(),
            finished: true,
            usage: accumulated_usage.clone(),
            tool_calls: if current_tool_calls.is_empty() {
                None
            } else {
                Some(current_tool_calls)
            },
        };
        let _ = tx.send(Ok(final_chunk)).await;
        accumulated_usage
    }
}

#[async_trait::async_trait]
impl LLMProvider for AnthropicProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing Anthropic completion request with {} messages",
            request.messages.len()
        );

        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            max_tokens,
            temperature,
            request.disable_thinking,
        )?;

        debug!(
            "Sending request to Anthropic API: model={}, max_tokens={}, temperature={}",
            request_body.model, request_body.max_tokens, request_body.temperature
        );

        let response = self
            .create_request_builder(false)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Anthropic API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!("Anthropic API error {}: {}", status, error_text));
        }

        let anthropic_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Anthropic response: {}", e))?;

        // Extract text content from the response
        let content = anthropic_response
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let usage = Usage {
            prompt_tokens: anthropic_response.usage.input_tokens,
            completion_tokens: anthropic_response.usage.output_tokens,
            total_tokens: anthropic_response.usage.input_tokens
                + anthropic_response.usage.output_tokens,
        };

        debug!(
            "Anthropic completion successful: {} tokens generated",
            usage.completion_tokens
        );

        Ok(CompletionResponse {
            content,
            usage,
            model: anthropic_response.model,
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        debug!(
            "Processing Anthropic streaming request with {} messages",
            request.messages.len()
        );

        let max_tokens = request.max_tokens.unwrap_or(self.max_tokens);
        let temperature = request.temperature.unwrap_or(self.temperature);

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
            max_tokens,
            temperature,
            request.disable_thinking,
        )?;

        debug!(
            "Sending streaming request to Anthropic API: model={}, max_tokens={}, temperature={}",
            request_body.model, request_body.max_tokens, request_body.temperature
        );

        // Debug: Log the full request body
        debug!(
            "Full request body: {}",
            serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| "Failed to serialize".to_string())
        );

        let response = self
            .create_request_builder(true)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send streaming request to Anthropic API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!("Anthropic API error {}: {}", status, error_text));
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
        // Claude models support native tool calling
        true
    }

    fn supports_cache_control(&self) -> bool {
        // Anthropic supports cache control
        true
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn temperature(&self) -> f32 {
        self.temperature
    }
}

// Anthropic API request/response structures

#[derive(Debug, Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: u32,
}

impl ThinkingConfig {
    fn enabled(budget_tokens: u32) -> Self {
        Self { thinking_type: "enabled".to_string(), budget_tokens }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    temperature: f32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: AnthropicToolInputSchema,
}

#[derive(Debug, Serialize)]
struct AnthropicToolInputSchema {
    #[serde(rename = "type")]
    schema_type: String,
    properties: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContent {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<crate::CacheControl>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "image")]
    Image {
        source: AnthropicImageSource,
    },
}

/// Image source for Anthropic API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String, // Always "base64"
    media_type: String,  // e.g., "image/png", "image/jpeg"
    data: String,        // Base64-encoded image data
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    model: String,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// Streaming response structures

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<AnthropicDelta>,
    #[serde(default)]
    error: Option<AnthropicError>,
    #[serde(default)]
    content_block: Option<AnthropicContent>,
    #[serde(default)]
    message: Option<AnthropicStreamMessage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamMessage {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    text: Option<String>,
    partial_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    error_type: String,
    #[allow(dead_code)]
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_conversion() {
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None, None).unwrap();

        let messages = vec![
            Message::new(
                MessageRole::System,
                "You are a helpful assistant.".to_string(),
            ),
            Message::new(MessageRole::User, "Hello!".to_string()),
            Message::new(MessageRole::Assistant, "Hi there!".to_string()),
        ];

        let (system, anthropic_messages) = provider.convert_messages(&messages).unwrap();

        assert_eq!(system, Some("You are a helpful assistant.".to_string()));
        assert_eq!(anthropic_messages.len(), 2);
        assert_eq!(anthropic_messages[0].role, "user");
        assert_eq!(anthropic_messages[1].role, "assistant");
    }

    #[test]
    fn test_request_body_creation() {
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-3-haiku-20240307".to_string()),
            None, // base_url
            Some(1000),
            Some(0.5),
            None,
            None,
            None,
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];

        let request_body = provider
            .create_request_body(&messages, None, false, 1000, 0.5, false)
            .unwrap();

        assert_eq!(request_body.model, "claude-3-haiku-20240307");
        assert_eq!(request_body.max_tokens, 1000);
        assert_eq!(request_body.temperature, 0.5);
        assert!(!request_body.stream);
        assert_eq!(request_body.messages.len(), 1);
        assert!(request_body.tools.is_none());
    }

    #[test]
    fn test_tool_conversion() {
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None, None).unwrap();

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

        let anthropic_tools = provider.convert_tools(&tools);

        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "get_weather");
        assert_eq!(anthropic_tools[0].description, "Get the current weather");
        assert_eq!(anthropic_tools[0].input_schema.schema_type, "object");
        assert!(anthropic_tools[0].input_schema.required.is_some());
        assert_eq!(
            anthropic_tools[0].input_schema.required.as_ref().unwrap()[0],
            "location"
        );
    }

    #[test]
    fn test_cache_control_serialization() {
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None, None).unwrap();

        // Test message WITHOUT cache_control
        let messages_without = vec![Message::new(MessageRole::User, "Hello".to_string())];
        let (_, anthropic_messages_without) = provider.convert_messages(&messages_without).unwrap();
        let json_without = serde_json::to_string(&anthropic_messages_without).unwrap();

        println!("Anthropic JSON without cache_control: {}", json_without);
        // Check if cache_control appears in the JSON
        if json_without.contains("cache_control") {
            println!("WARNING: JSON contains 'cache_control' field when not configured!");
            assert!(
                !json_without.contains("\"cache_control\":null"),
                "JSON should not contain 'cache_control: null'"
            );
        }

        // Test message WITH cache_control
        let messages_with = vec![Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            crate::CacheControl::ephemeral(),
        )];
        let (_, anthropic_messages_with) = provider.convert_messages(&messages_with).unwrap();
        let json_with = serde_json::to_string(&anthropic_messages_with).unwrap();

        println!("Anthropic JSON with cache_control: {}", json_with);
        assert!(
            json_with.contains("cache_control"),
            "JSON should contain 'cache_control' field when configured"
        );
        assert!(
            json_with.contains("ephemeral"),
            "JSON should contain 'ephemeral' type"
        );

        // The key assertion: when cache_control is None, it should not appear in JSON
        assert!(
            !json_without.contains("cache_control") || !json_without.contains("null"),
            "JSON should not contain 'cache_control' field or null values when not configured"
        );
    }

    #[test]
    fn test_thinking_parameter_serialization() {
        // Test WITHOUT thinking parameter
        let provider_without = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
            None, // base_url
            Some(1000),
            Some(0.5),
            None,
            None,
            None, // No thinking budget
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];
        let request_without = provider_without
            .create_request_body(&messages, None, false, 1000, 0.5, false)
            .unwrap();
        let json_without = serde_json::to_string(&request_without).unwrap();
        assert!(!json_without.contains("thinking"), "JSON should not contain 'thinking' field when not configured");

        // Test WITH thinking parameter - max_tokens must be > budget_tokens + 1024
        // Using budget=10000 requires max_tokens > 11024
        let provider_with = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
            None, // base_url
            Some(20000),  // Sufficient for thinking budget
            Some(0.5),
            None,
            None,
            Some(10000), // With thinking budget
        )
        .unwrap();

        let request_with = provider_with
            .create_request_body(&messages, None, false, 20000, 0.5, false)
            .unwrap();
        let json_with = serde_json::to_string(&request_with).unwrap();
        assert!(json_with.contains("thinking"), "JSON should contain 'thinking' field when configured");
        assert!(json_with.contains("\"type\":\"enabled\""), "JSON should contain type: enabled");
        assert!(json_with.contains("\"budget_tokens\":10000"), "JSON should contain budget_tokens: 10000");

        // Test WITH thinking parameter but INSUFFICIENT max_tokens - thinking should be disabled
        let request_insufficient = provider_with
            .create_request_body(&messages, None, false, 5000, 0.5, false)  // Less than budget + 1024
            .unwrap();
        let json_insufficient = serde_json::to_string(&request_insufficient).unwrap();
        assert!(!json_insufficient.contains("thinking"), "JSON should NOT contain 'thinking' field when max_tokens is insufficient");
    }

    #[test]
    fn test_disable_thinking_flag() {
        // Test that disable_thinking=true prevents thinking even with sufficient max_tokens
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
            None, // base_url
            Some(20000),
            Some(0.5),
            None,
            None,
            Some(10000), // With thinking budget
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];
        
        // With disable_thinking=false, thinking should be enabled (max_tokens is sufficient)
        let request_with_thinking = provider
            .create_request_body(&messages, None, false, 20000, 0.5, false)
            .unwrap();
        let json_with = serde_json::to_string(&request_with_thinking).unwrap();
        assert!(json_with.contains("thinking"), "JSON should contain 'thinking' field when not disabled");

        // With disable_thinking=true, thinking should be disabled even with sufficient max_tokens
        let request_without_thinking = provider
            .create_request_body(&messages, None, false, 20000, 0.5, true)
            .unwrap();
        let json_without = serde_json::to_string(&request_without_thinking).unwrap();
        assert!(!json_without.contains("thinking"), "JSON should NOT contain 'thinking' field when explicitly disabled");
    }

    #[test]
    fn test_thinking_content_block_deserialization() {
        // Test that we can deserialize a response containing a "thinking" content block
        // This is what Anthropic returns when extended thinking is enabled
        let json_response = r#"{
            "content": [
                {"type": "thinking", "thinking": "Let me analyze this...", "signature": "abc123"},
                {"type": "text", "text": "Here is my response."}
            ],
            "model": "claude-sonnet-4-5",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }"#;

        let response: AnthropicResponse = serde_json::from_str(json_response)
            .expect("Should be able to deserialize response with thinking block");
        
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.model, "claude-sonnet-4-5");
        
        // Extract only text content (thinking should be filtered out)
        let text_content: Vec<_> = response.content.iter().filter_map(|c| match c {
            AnthropicContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        
        assert_eq!(text_content.len(), 1);
        assert_eq!(text_content[0], "Here is my response.");
    }

    #[test]
    fn test_base_url_configuration() {
        // Test with default base_url (None)
        let provider_default = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None, // base_url - should use default
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(provider_default.base_url, DEFAULT_ANTHROPIC_BASE_URL);

        // Test with custom base_url
        let custom_url = "https://custom.api.example.com/v1";
        let provider_custom = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            Some(custom_url.to_string()),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(provider_custom.base_url, custom_url);

        // Test new_with_name with custom base_url
        let provider_named = AnthropicProvider::new_with_name(
            "anthropic.custom".to_string(),
            "test-key".to_string(),
            Some("claude-3-haiku-20240307".to_string()),
            Some(custom_url.to_string()),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(provider_named.base_url, custom_url);
        assert_eq!(provider_named.name, "anthropic.custom");
    }
}
