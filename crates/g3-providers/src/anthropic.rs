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
//!         Some("claude-3-5-sonnet-20241022".to_string()), // Optional: defaults to claude-3-5-sonnet-20241022
//!         Some(4096),  // Optional: max tokens
//!         Some(0.1),   // Optional: temperature
//!     )?;
//!
//!     // Create a completion request
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message {
//!                 role: MessageRole::System,
//!                 content: "You are a helpful assistant.".to_string(),
//!             },
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
//!         None, None, None,
//!     )?;
//!
//!     let request = CompletionRequest {
//!         messages: vec![
//!             Message {
//!                 role: MessageRole::User,
//!                 content: "Write a short story about a robot.".to_string(),
//!             },
//!         ],
//!         max_tokens: Some(1000),
//!         temperature: Some(0.7),
//!         stream: true,
//!         tools: None,
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
use tracing::{debug, error, info, warn};

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl AnthropicProvider {
    pub fn new(
        api_key: String,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let model = model.unwrap_or_else(|| "claude-3-5-sonnet-20241022".to_string());
        
        info!("Initialized Anthropic provider with model: {}", model);

        Ok(Self {
            client,
            api_key,
            model,
            max_tokens: max_tokens.unwrap_or(4096),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    fn create_request_builder(&self, streaming: bool) -> RequestBuilder {
        let mut builder = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            // Anthropic beta 1m context window. Enable if needed. It costs extra, so check first.
            // .header("anthropic-beta", "context-1m-2025-08-07")
            .header("content-type", "application/json");
        if streaming {
            builder = builder.header("accept", "text/event-stream");
        }

        builder
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
                if let Ok(schema_obj) = serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(tool.input_schema.clone()) {
                    if let Some(properties) = schema_obj.get("properties") {
                        schema.properties = properties.clone();
                    }
                    if let Some(required) = schema_obj.get("required") {
                        if let Ok(required_vec) = serde_json::from_value::<Vec<String>>(required.clone()) {
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

    fn convert_messages(&self, messages: &[Message]) -> Result<(Option<String>, Vec<AnthropicMessage>)> {
        let mut system_message = None;
        let mut anthropic_messages = Vec::new();

        for message in messages {
            match message.role {
                MessageRole::System => {
                    if system_message.is_some() {
                        warn!("Multiple system messages found, using the last one");
                    }
                    system_message = Some(message.content.clone());
                }
                MessageRole::User => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: vec![AnthropicContent::Text {
                            text: message.content.clone(),
                        }],
                    });
                }
                MessageRole::Assistant => {
                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: vec![AnthropicContent::Text {
                            text: message.content.clone(),
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
    ) -> Result<AnthropicRequest> {
        let (system, anthropic_messages) = self.convert_messages(messages)?;

        if anthropic_messages.is_empty() {
            return Err(anyhow!("At least one user or assistant message is required"));
        }

        // Convert tools if provided
        let anthropic_tools = tools.map(|t| self.convert_tools(t));

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            temperature,
            messages: anthropic_messages,
            system,
            tools: anthropic_tools,
            stream: streaming,
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
        let mut actual_completion_tokens: u32 = 0; // Track actual completion tokens
        
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
                                let valid_bytes = byte_buffer.drain(..valid_up_to).collect::<Vec<_>>();
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

                        // Parse Server-Sent Events format
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("Received stream completion marker");
                                let final_chunk = CompletionChunk {
                                    content: String::new(),
                                    finished: true,
                                    usage: accumulated_usage.as_ref().map(|u| Usage {
                                        prompt_tokens: u.prompt_tokens,
                                        // Use actual completion tokens if we tracked them, otherwise use the estimate
                                        completion_tokens: if actual_completion_tokens > 0 { actual_completion_tokens } else { u.completion_tokens },
                                        total_tokens: u.prompt_tokens + if actual_completion_tokens > 0 { actual_completion_tokens } else { u.completion_tokens },
                                    }),
                                    tool_calls: if current_tool_calls.is_empty() { None } else { Some(current_tool_calls.clone()) },
                                };
                                if tx.send(Ok(final_chunk)).await.is_err() {
                                    debug!("Receiver dropped, stopping stream");
                                }
                                return accumulated_usage;
                            }

                            debug!("Raw Claude API JSON: {}", data);

                            match serde_json::from_str::<AnthropicStreamEvent>(data) {
                                Ok(event) => {
                                    debug!("Parsed event type: {}, event: {:?}", event.event_type, event);
                                    
                                    match event.event_type.as_str() {
                                        "message_start" => {
                                            // Extract usage data from message_start event
                                            if let Some(message) = event.message {
                                                if let Some(usage) = message.usage {
                                                    accumulated_usage = Some(Usage {
                                                        prompt_tokens: usage.input_tokens,
                                                        completion_tokens: usage.output_tokens,
                                                        total_tokens: usage.input_tokens + usage.output_tokens,
                                                    });
                                                    debug!("Captured initial usage from message_start - prompt: {}, completion: {} (estimated), total: {}",
                                                        usage.input_tokens,
                                                        usage.output_tokens,
                                                        usage.input_tokens + usage.output_tokens);
                                                }
                                            }
                                        }
                                        "content_block_start" => {
                                            debug!("Received content_block_start event: {:?}", event);
                                            if let Some(content_block) = event.content_block {
                                                match content_block {
                                                    AnthropicContent::ToolUse { id, name, input } => {
                                                        debug!("Found tool use in content_block_start: id={}, name={}, input={:?}", id, name, input);
                                                        
                                                        // For native tool calls, create the tool call immediately if we have complete args
                                                        // If args are empty, we'll wait for partial_json to accumulate them
                                                        let tool_call = ToolCall {
                                                            id: id.clone(),
                                                            tool: name.clone(),
                                                            args: input.clone(),
                                                        };
                                                        
                                                        // Check if we already have complete arguments
                                                        if !input.is_null() && input != serde_json::Value::Object(serde_json::Map::new()) {
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
                                                        debug!("Non-tool content block: {:?}", content_block);
                                                    }
                                                }
                                            }
                                        }
                                        "content_block_delta" => {
                                            if let Some(delta) = event.delta {
                                                if let Some(text) = delta.text {
                                                    // Track actual completion tokens (rough estimate: 4 chars per token)
                                                    actual_completion_tokens += (text.len() as f32 / 4.0).ceil() as u32;
                                                    
                                                    debug!("Sending text chunk of length {}: '{}'", text.len(), text);
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
                                                    debug!("Received partial JSON: {}", partial_json);
                                                    partial_tool_json.push_str(&partial_json);
                                                    debug!("Accumulated tool JSON: {}", partial_tool_json);
                                                }
                                            }
                                        }
                                        "message_delta" => {
                                            // Check if message_delta contains updated usage data
                                            if let Some(delta) = event.delta {
                                                if let Some(usage) = delta.usage {
                                                    accumulated_usage = Some(Usage {
                                                        prompt_tokens: usage.input_tokens,
                                                        completion_tokens: usage.output_tokens,
                                                        total_tokens: usage.input_tokens + usage.output_tokens,
                                                    });
                                                    debug!("Updated usage from message_delta - prompt: {}, completion: {}, total: {}", usage.input_tokens, usage.output_tokens, usage.input_tokens + usage.output_tokens);
                                                }
                                            }
                                        }
                                        "content_block_stop" => {
                                            // Tool call block is complete - now parse the accumulated JSON
                                            if !current_tool_calls.is_empty() && !partial_tool_json.is_empty() {
                                                debug!("Parsing complete tool JSON: {}", partial_tool_json);
                                                
                                                // Parse the accumulated JSON and update the last tool call
                                                if let Ok(parsed_args) = serde_json::from_str::<serde_json::Value>(&partial_tool_json) {
                                                    if let Some(last_tool) = current_tool_calls.last_mut() {
                                                        last_tool.args = parsed_args;
                                                        debug!("Updated tool call with complete args: {:?}", last_tool);
                                                    }
                                                } else {
                                                    debug!("Failed to parse accumulated JSON: {}", partial_tool_json);
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
                                            }
                                        }
                                        "message_stop" => {
                                            debug!("Received message_stop event: {:?}", event);
                                            
                                            // Check if message_stop contains final usage data
                                            if let Some(message) = event.message {
                                                if let Some(usage) = message.usage {
                                                    // Update with final accurate usage data from message_stop
                                                    // This should have the actual completion token count
                                                    accumulated_usage = Some(Usage {
                                                        prompt_tokens: usage.input_tokens,
                                                        // Prefer the actual output_tokens from message_stop if available
                                                        // Otherwise use our tracked count, and as last resort the initial estimate
                                                        completion_tokens: if usage.output_tokens > 0 {
                                                            usage.output_tokens
                                                        } else if actual_completion_tokens > 0 {
                                                            actual_completion_tokens
                                                        } else { usage.output_tokens },
                                                        total_tokens: usage.input_tokens + usage.output_tokens,
                                                    });
                                                    debug!("Updated with final usage from message_stop - prompt: {}, completion: {}, total: {}",
                                                        usage.input_tokens,
                                                        usage.output_tokens,
                                                        usage.input_tokens + usage.output_tokens);
                                                }
                                            }
                                            
                                            let final_chunk = CompletionChunk {
                                                content: String::new(),
                                                finished: true,
                                                usage: accumulated_usage.as_ref().map(|u| Usage {
                                                    prompt_tokens: u.prompt_tokens,
                                                    // Use actual completion tokens if we tracked them and they're higher
                                                    completion_tokens: if actual_completion_tokens > u.completion_tokens {
                                                        actual_completion_tokens
                                                    } else {
                                                        u.completion_tokens
                                                    },
                                                    total_tokens: u.prompt_tokens + u32::max(actual_completion_tokens, u.completion_tokens),
                                                }),
                                                tool_calls: if current_tool_calls.is_empty() { None } else { Some(current_tool_calls.clone()) },
                                            };
                                            if tx.send(Ok(final_chunk)).await.is_err() {
                                                debug!("Receiver dropped, stopping stream");
                                            }
                                            return accumulated_usage;
                                        }
                                        "error" => {
                                            if let Some(error) = event.error {
                                                error!("Anthropic API error: {:?}", error);
                                                let _ = tx
                                                    .send(Err(anyhow!("Anthropic API error: {:?}", error)))
                                                    .await;
                                                return accumulated_usage;
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
                    return accumulated_usage;
                }
            }
        }

        // Send final chunk if we haven't already
        let final_chunk = CompletionChunk {
            content: String::new(),
            finished: true,
            usage: accumulated_usage.as_ref().map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: if actual_completion_tokens > u.completion_tokens {
                    actual_completion_tokens
                } else {
                    u.completion_tokens
                },
                total_tokens: u.prompt_tokens + u32::max(actual_completion_tokens, u.completion_tokens),
            }),
            tool_calls: if current_tool_calls.is_empty() { None } else { Some(current_tool_calls) },
        };
        let _ = tx.send(Ok(final_chunk)).await;
        
        // Log final usage for debugging
        if let Some(ref usage) = accumulated_usage {
            info!("Anthropic stream completed with final usage - prompt: {}, completion: {}, total: {}",
                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens);
        } else {
            warn!("Anthropic stream completed without usage data - token accounting will fall back to estimation");
        }
        
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
            temperature
        )?;

        debug!("Sending request to Anthropic API: model={}, max_tokens={}, temperature={}", 
               request_body.model, request_body.max_tokens, request_body.temperature);

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
                AnthropicContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let usage = Usage {
            prompt_tokens: anthropic_response.usage.input_tokens,
            completion_tokens: anthropic_response.usage.output_tokens,
            total_tokens: anthropic_response.usage.input_tokens + anthropic_response.usage.output_tokens,
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
            temperature
        )?;

        debug!("Sending streaming request to Anthropic API: model={}, max_tokens={}, temperature={}", 
               request_body.model, request_body.max_tokens, request_body.temperature);
        
        // Debug: Log the full request body
        debug!("Full request body: {}", serde_json::to_string_pretty(&request_body).unwrap_or_else(|_| "Failed to serialize".to_string()));

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
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // Claude models support native tool calling
        true
    }
}

// Anthropic API request/response structures

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
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
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
    #[serde(default)]
    usage: Option<AnthropicUsage>,
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
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
        ).unwrap();

        let messages = vec![
            Message {
                role: MessageRole::System,
                content: "You are a helpful assistant.".to_string(),
            },
            Message {
                role: MessageRole::User,
                content: "Hello!".to_string(),
            },
            Message {
                role: MessageRole::Assistant,
                content: "Hi there!".to_string(),
            },
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
            Some(1000),
            Some(0.5),
        ).unwrap();

        let messages = vec![
            Message {
                role: MessageRole::User,
                content: "Test message".to_string(),
            },
        ];

        let request_body = provider
            .create_request_body(&messages, None, false, 1000, 0.5)
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
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
        ).unwrap();

        let tools = vec![
            Tool {
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
            },
        ];

        let anthropic_tools = provider.convert_tools(&tools);

        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0].name, "get_weather");
        assert_eq!(anthropic_tools[0].description, "Get the current weather");
        assert_eq!(anthropic_tools[0].input_schema.schema_type, "object");
        assert!(anthropic_tools[0].input_schema.required.is_some());
        assert_eq!(anthropic_tools[0].input_schema.required.as_ref().unwrap()[0], "location");
    }
}
