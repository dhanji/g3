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

/// Azure AI provider for Claude models
/// 
/// Azure AI Model Catalog exposes Claude via the native Anthropic Messages API format,
/// but uses `api-key` header for authentication instead of `x-api-key`.
#[derive(Clone)]
pub struct AzureProvider {
    client: Client,
    endpoint: String,
    api_key: String,
    model: String,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    name: String,
}

impl AzureProvider {
    pub fn new(
        endpoint: String,
        api_key: String,
        deployment: String,
        _api_version: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        Self::new_with_name(
            "azure".to_string(),
            endpoint,
            api_key,
            deployment,
            _api_version,
            max_tokens,
            temperature,
        )
    }

    pub fn new_with_name(
        name: String,
        endpoint: String,
        api_key: String,
        deployment: String,
        _api_version: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        // Normalize endpoint - remove trailing slash if present
        let endpoint = endpoint.trim_end_matches('/').to_string();
        
        Ok(Self {
            client: Client::new(),
            endpoint,
            api_key,
            model: deployment, // deployment name is used as model
            max_tokens,
            temperature,
            name,
        })
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> serde_json::Value {
        // Convert messages to Anthropic format (system message separate)
        let (system_content, anthropic_messages) = convert_messages_to_anthropic(messages);
        
        let mut body = json!({
            "model": &self.model,
            "messages": anthropic_messages,
            "max_tokens": max_tokens.or(self.max_tokens).unwrap_or(4096),
            "stream": stream,
        });

        if let Some(system) = system_content {
            body["system"] = json!(system);
        }

        if let Some(temperature) = temperature.or(self.temperature) {
            body["temperature"] = json!(temperature);
        }

        if let Some(tools) = tools {
            if !tools.is_empty() {
                body["tools"] = json!(convert_tools_to_anthropic(tools));
            }
        }

        body
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut accumulated_content = String::new();
        let mut accumulated_usage: Option<Usage> = None;
        let mut current_tool_calls: Vec<AnthropicStreamingToolCall> = Vec::new();

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

                    // Process complete lines (SSE format)
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
                                    content: accumulated_content.clone(),
                                    finished: true,
                                    tool_calls,
                                    usage: accumulated_usage.clone(),
                                };
                                let _ = tx.send(Ok(final_chunk)).await;
                                return accumulated_usage;
                            }

                            // Parse the JSON data - Anthropic streaming format
                            if let Ok(event) = serde_json::from_str::<AnthropicStreamEvent>(data) {
                                match event.event_type.as_str() {
                                    "content_block_delta" => {
                                        if let Some(delta) = event.delta {
                                            if let Some(text) = delta.text {
                                                accumulated_content.push_str(&text);
                                                let chunk = CompletionChunk {
                                                    content: text,
                                                    finished: false,
                                                    tool_calls: None,
                                                    usage: None,
                                                };
                                                if tx.send(Ok(chunk)).await.is_err() {
                                                    debug!("Receiver dropped, stopping stream");
                                                    return accumulated_usage;
                                                }
                                            }
                                            // Handle tool use delta
                                            if let Some(partial_json) = delta.partial_json {
                                                if let Some(tool_call) = current_tool_calls.last_mut() {
                                                    tool_call.arguments.push_str(&partial_json);
                                                }
                                            }
                                        }
                                    }
                                    "content_block_start" => {
                                        if let Some(content_block) = event.content_block {
                                            if content_block.block_type == "tool_use" {
                                                current_tool_calls.push(AnthropicStreamingToolCall {
                                                    id: content_block.id,
                                                    name: content_block.name,
                                                    arguments: String::new(),
                                                });
                                            }
                                        }
                                    }
                                    "message_delta" => {
                                        if let Some(usage) = event.usage {
                                            accumulated_usage = Some(Usage {
                                                prompt_tokens: usage.input_tokens.unwrap_or(0),
                                                completion_tokens: usage.output_tokens.unwrap_or(0),
                                                total_tokens: usage.input_tokens.unwrap_or(0)
                                                    + usage.output_tokens.unwrap_or(0),
                                            });
                                        }
                                    }
                                    "message_stop" => {
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
                                            content: accumulated_content.clone(),
                                            finished: true,
                                            tool_calls,
                                            usage: accumulated_usage.clone(),
                                        };
                                        let _ = tx.send(Ok(final_chunk)).await;
                                        return accumulated_usage;
                                    }
                                    _ => {}
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
impl LLMProvider for AzureProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing Azure/Anthropic completion request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            request.max_tokens,
            request.temperature,
        );

        debug!("Sending request to Azure endpoint: {}", self.endpoint);

        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "Azure API error {}: {}",
                status,
                error_text
            ));
        }

        let anthropic_response: AnthropicResponse = response.json().await?;

        // Extract text content from response
        let content = anthropic_response
            .content
            .iter()
            .filter_map(|block| {
                if block.content_type == "text" {
                    block.text.clone()
                } else {
                    None
                }
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
            "Azure completion successful: {} tokens generated",
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
            "Processing Azure/Anthropic streaming request with {} messages",
            request.messages.len()
        );

        let body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
            request.max_tokens,
            request.temperature,
        );

        debug!("Sending streaming request to Azure endpoint: {}", self.endpoint);

        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!(
                "Azure API error {}: {}",
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
        true
    }

    fn supports_cache_control(&self) -> bool {
        // Azure Claude supports Anthropic's cache control
        true
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens.unwrap_or(16000)
    }

    fn temperature(&self) -> f32 {
        self.temperature.unwrap_or(0.1)
    }
}

/// Convert messages to Anthropic format
/// Returns (system_content, messages) where system is extracted separately
fn convert_messages_to_anthropic(messages: &[Message]) -> (Option<String>, Vec<serde_json::Value>) {
    let mut system_content: Option<String> = None;
    let mut anthropic_messages = Vec::new();

    for msg in messages {
        match msg.role {
            MessageRole::System => {
                // Anthropic puts system message at top level, not in messages array
                if let Some(ref mut existing) = system_content {
                    existing.push_str("\n\n");
                    existing.push_str(&msg.content);
                } else {
                    system_content = Some(msg.content.clone());
                }
            }
            MessageRole::User => {
                anthropic_messages.push(json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            MessageRole::Assistant => {
                anthropic_messages.push(json!({
                    "role": "assistant",
                    "content": msg.content,
                }));
            }
        }
    }

    (system_content, anthropic_messages)
}

/// Convert tools to Anthropic format
fn convert_tools_to_anthropic(tools: &[Tool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect()
}

// Anthropic API response structures
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    input: Option<serde_json::Value>,
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
    delta: Option<StreamDelta>,
    content_block: Option<StreamContentBlock>,
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    text: Option<String>,
    partial_json: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

// Streaming tool call accumulator
#[derive(Debug, Default)]
struct AnthropicStreamingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl AnthropicStreamingToolCall {
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
