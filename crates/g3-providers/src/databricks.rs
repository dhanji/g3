//! Databricks LLM provider implementation for the g3-providers crate.
//!
//! This module provides an implementation of the `LLMProvider` trait for Databricks Foundation Model APIs,
//! supporting both completion and streaming modes with OAuth authentication.
//!
//! # Features
//!
//! - Support for Databricks Foundation Models (databricks-claude-sonnet-4, databricks-meta-llama-3-3-70b-instruct, etc.)
//! - Both completion and streaming response modes
//! - OAuth authentication with automatic token refresh
//! - Token-based authentication as fallback
//! - Native tool calling support for compatible models
//! - Automatic model discovery from Databricks workspace
//!
//! # Usage
//!
//! ```rust,no_run
//! use g3_providers::{DatabricksProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Create the provider with OAuth (recommended)
//!     let provider = DatabricksProvider::from_oauth(
//!         "https://your-workspace.cloud.databricks.com".to_string(),
//!         "databricks-claude-sonnet-4".to_string(),
//!         None, // Optional: max tokens
//!         None, // Optional: temperature
//!     ).await?;
//!
//!     // Or create with token
//!     let provider = DatabricksProvider::from_token(
//!         "https://your-workspace.cloud.databricks.com".to_string(),
//!         "your-databricks-token".to_string(),
//!         "databricks-claude-sonnet-4".to_string(),
//!         None,
//!         None,
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

const DEFAULT_CLIENT_ID: &str = "databricks-cli";
const DEFAULT_REDIRECT_URL: &str = "http://localhost:8020";
const DEFAULT_SCOPES: &[&str] = &["all-apis", "offline_access"];
const DEFAULT_TIMEOUT_SECS: u64 = 600;

pub const DATABRICKS_DEFAULT_MODEL: &str = "databricks-claude-sonnet-4";
pub const DATABRICKS_KNOWN_MODELS: &[&str] = &[
    "databricks-claude-3-7-sonnet",
    "databricks-meta-llama-3-3-70b-instruct",
    "databricks-meta-llama-3-1-405b-instruct",
    "databricks-dbrx-instruct",
    "databricks-mixtral-8x7b-instruct",
];

#[derive(Debug, Clone)]
pub enum DatabricksAuth {
    Token(String),
    OAuth {
        host: String,
        client_id: String,
        redirect_url: String,
        scopes: Vec<String>,
        cached_token: Option<String>,
    },
}

impl DatabricksAuth {
    pub fn oauth(host: String) -> Self {
        Self::OAuth {
            host,
            client_id: DEFAULT_CLIENT_ID.to_string(),
            redirect_url: DEFAULT_REDIRECT_URL.to_string(),
            scopes: DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect(),
            cached_token: None,
        }
    }

    pub fn token(token: String) -> Self {
        Self::Token(token)
    }

    async fn get_token(&mut self) -> Result<String> {
        match self {
            DatabricksAuth::Token(token) => Ok(token.clone()),
            DatabricksAuth::OAuth {
                host,
                client_id,
                redirect_url,
                scopes,
                cached_token,
            } => {
                // Use the OAuth implementation with automatic refresh
                let token =
                    crate::oauth::get_oauth_token_async(host, client_id, redirect_url, scopes)
                        .await?;
                // Cache the token for potential reuse within the same session
                *cached_token = Some(token.clone());
                Ok(token)
            }
        }
    }

    /// Force a token refresh by clearing any cached token
    /// This is useful when we get a 403 Invalid Token error
    pub fn clear_cached_token(&mut self) {
        if let DatabricksAuth::OAuth { cached_token, .. } = self {
            *cached_token = None;
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatabricksProvider {
    client: Client,
    host: String,
    auth: DatabricksAuth,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl DatabricksProvider {
    pub fn from_token(
        host: String,
        token: String,
        model: String,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        info!(
            "Initialized Databricks provider with model: {} on host: {}",
            model, host
        );

        Ok(Self {
            client,
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::token(token),
            model,
            max_tokens: max_tokens.unwrap_or(50000),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    pub async fn from_oauth(
        host: String,
        model: String,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        info!(
            "Initialized Databricks provider with OAuth for model: {} on host: {}",
            model, host
        );

        Ok(Self {
            client,
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::oauth(host.clone()),
            model,
            max_tokens: max_tokens.unwrap_or(50000),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    async fn create_request_builder(&mut self, streaming: bool) -> Result<RequestBuilder> {
        let token = self.auth.get_token().await?;

        let mut builder = self
            .client
            .post(&format!(
                "{}/serving-endpoints/{}/invocations",
                self.host, self.model
            ))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json");

        if streaming {
            builder = builder.header("Accept", "text/event-stream");
        }

        Ok(builder)
    }

    fn convert_tools(&self, tools: &[Tool]) -> Vec<DatabricksTool> {
        tools
            .iter()
            .map(|tool| DatabricksTool {
                r#type: "function".to_string(),
                function: DatabricksFunction {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                },
            })
            .collect()
    }

    fn convert_messages(&self, messages: &[Message]) -> Result<Vec<DatabricksMessage>> {
        let mut databricks_messages = Vec::new();

        for message in messages {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            };

            databricks_messages.push(DatabricksMessage {
                role: role.to_string(),
                content: Some(message.content.clone()),
                tool_calls: None, // Only used in responses, not requests
            });
        }

        if databricks_messages.is_empty() {
            return Err(anyhow!("At least one message is required"));
        }

        Ok(databricks_messages)
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        streaming: bool,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<DatabricksRequest> {
        let databricks_messages = self.convert_messages(messages)?;

        // Convert tools if provided
        let databricks_tools = tools.map(|t| self.convert_tools(t));

        let request = DatabricksRequest {
            messages: databricks_messages,
            max_tokens,
            temperature,
            tools: databricks_tools,
            stream: streaming,
        };

        Ok(request)
    }

    async fn parse_streaming_response(
        &self,
        mut stream: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Unpin,
        tx: mpsc::Sender<Result<CompletionChunk>>,
    ) -> Option<Usage> {
        let mut buffer = String::new();
        let mut current_tool_calls: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new(); // index -> (id, name, args)
        let mut incomplete_data_line = String::new(); // Buffer for incomplete data: lines
        let accumulated_usage: Option<Usage> = None;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    // Debug: Log raw bytes received
                    debug!("Raw SSE bytes received: {} bytes", chunk.len());

                    let chunk_str = match std::str::from_utf8(&chunk) {
                        Ok(s) => {
                            // Debug: Log raw string content (truncated for large chunks)
                            if s.len() > 1000 {
                                debug!(
                                    "Raw SSE string content (first 500 chars): {:?}...",
                                    &s[..500]
                                );
                            } else {
                                debug!("Raw SSE string content: {:?}", s);
                            }
                            s
                        }
                        Err(e) => {
                            error!("Invalid UTF-8 in stream chunk: {}", e);
                            let _ = tx
                                .send(Err(anyhow!("Invalid UTF-8 in stream chunk: {}", e)))
                                .await;
                            return accumulated_usage;
                        }
                    };

                    buffer.push_str(chunk_str);

                    // Process complete lines, but handle incomplete data: lines specially
                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim().to_string();
                        buffer.drain(..line_end + 1);

                        if line.is_empty() {
                            continue;
                        }

                        // Check if we have an incomplete data line from previous chunk
                        let line = if !incomplete_data_line.is_empty() {
                            // We had an incomplete data: line, append this line to it
                            let complete_line = format!("{}{}", incomplete_data_line, line);
                            incomplete_data_line.clear();
                            complete_line
                        } else {
                            line
                        };

                        // Check if this is a data: line that might be incomplete
                        // SSE format requires double newline after data, so if we don't see another newline
                        // after this one in the buffer, and it's a data: line, it might be incomplete
                        if line.starts_with("data: ") {
                            // Check if there's a complete SSE event (should have double newline after data)
                            // But for streaming, single newline is often used, so we need to be careful
                            // The safest approach is to try parsing and if it fails due to incomplete JSON,
                            // we'll handle it below
                        }

                        // Debug: Log each SSE line (truncated for large lines)
                        if line.len() > 1000 {
                            debug!("SSE line (first 500 chars): {:?}...", &line[..500]);
                        } else {
                            debug!("SSE line: {:?}", line);
                        }

                        // Parse Server-Sent Events format
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                debug!("Received stream completion marker");
                                let final_tool_calls: Vec<ToolCall> = current_tool_calls
                                    .values()
                                    .map(|(id, name, args)| ToolCall {
                                        id: id.clone(),
                                        tool: name.clone(),
                                        args: serde_json::from_str(args).unwrap_or(
                                            serde_json::Value::Object(serde_json::Map::new()),
                                        ),
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

                            // Debug: Log every raw JSON payload from Databricks API (truncated for large payloads)
                            if data.len() > 1000 {
                                debug!(
                                    "Raw Databricks SSE JSON payload (first 500 chars): {}...",
                                    &data[..500]
                                );
                            } else {
                                debug!("Raw Databricks SSE JSON payload: {}", data);
                            }

                            match serde_json::from_str::<DatabricksStreamChunk>(data) {
                                Ok(chunk) => {
                                    debug!("Successfully parsed Databricks stream chunk");

                                    // Handle different types of chunks
                                    if let Some(choices) = chunk.choices {
                                        for choice in choices {
                                            if let Some(delta) = choice.delta {
                                                // Handle text content
                                                if let Some(content) = delta.content {
                                                    debug!("Sending text chunk: '{}'", content);
                                                    let chunk = CompletionChunk {
                                                        content,
                                                        finished: false,
                                                        usage: None,
                                                        tool_calls: None,
                                                    };
                                                    if tx.send(Ok(chunk)).await.is_err() {
                                                        debug!("Receiver dropped, stopping stream");
                                                        return accumulated_usage;
                                                    }
                                                }

                                                // Handle tool calls - accumulate across chunks
                                                if let Some(tool_calls) = delta.tool_calls {
                                                    debug!(
                                                        "Processing {} tool call deltas",
                                                        tool_calls.len()
                                                    );
                                                    for tool_call in tool_calls {
                                                        let index = tool_call.index.unwrap_or(0);
                                                        debug!("Tool call delta for index {}: id={:?}, name='{}', args_len={}",
                                                               index, tool_call.id, tool_call.function.name, tool_call.function.arguments.len());

                                                        let entry = current_tool_calls
                                                            .entry(index)
                                                            .or_insert_with(|| {
                                                                (
                                                                    String::new(),
                                                                    String::new(),
                                                                    String::new(),
                                                                )
                                                            });

                                                        // Update ID if provided
                                                        if let Some(id) = tool_call.id {
                                                            debug!("Updating tool call {} ID from '{}' to '{}'", index, entry.0, id);
                                                            entry.0 = id;
                                                        }

                                                        // Update name if provided and not empty
                                                        if !tool_call.function.name.is_empty() {
                                                            debug!("Updating tool call {} name from '{}' to '{}'", index, entry.1, tool_call.function.name);
                                                            entry.1 = tool_call.function.name;
                                                        }

                                                        // Append arguments
                                                        debug!("Appending {} chars to tool call {} args (current len: {})",
                                                               tool_call.function.arguments.len(), index, entry.2.len());
                                                        entry.2.push_str(
                                                            &tool_call.function.arguments,
                                                        );

                                                        debug!("Accumulated tool call {}: id='{}', name='{}', args_len={}",
                                                               index, entry.0, entry.1, entry.2.len());

                                                        // Debug: Show a sample of the accumulated args if they're getting long
                                                        if entry.2.len() > 100 {
                                                            debug!("Tool call {} args sample (first 100 chars): {}", index, &entry.2[..100]);
                                                        } else if !entry.2.is_empty() {
                                                            debug!(
                                                                "Tool call {} full args: {}",
                                                                index, entry.2
                                                            );
                                                        }
                                                    }
                                                }
                                            }

                                            // Check if this choice is finished
                                            if choice.finish_reason.is_some() {
                                                debug!(
                                                    "Choice finished with reason: {:?}",
                                                    choice.finish_reason
                                                );

                                                // Convert accumulated tool calls to final format
                                                let final_tool_calls: Vec<ToolCall> = current_tool_calls.values()
                                                    .filter(|(_, name, _)| !name.is_empty()) // Only include tool calls with names
                                                    .map(|(id, name, args)| {
                                                        debug!("Converting tool call: id='{}', name='{}', args_len={}", id, name, args.len());
                                                        ToolCall {
                                                            id: if id.is_empty() { format!("tool_{}", name) } else { id.clone() },
                                                            tool: name.clone(),
                                                            args: serde_json::from_str(args).unwrap_or_else(|e| {
                                                                debug!("Failed to parse tool args (len={}): {}", args.len(), e);
                                                                // For debugging, log a sample of the args if they're very long
                                                                if args.len() > 1000 {
                                                                    debug!("Tool args sample (first 500 chars): {}", &args[..500]);
                                                                } else {
                                                                    debug!("Full tool args: {}", args);
                                                                }
                                                                serde_json::Value::Object(serde_json::Map::new())
                                                            }),
                                                        }
                                                    })
                                                    .collect();

                                                debug!(
                                                    "Final tool calls count: {}",
                                                    final_tool_calls.len()
                                                );

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
                                    }
                                }
                                Err(e) => {
                                    // Check if this is likely an incomplete JSON due to line splitting
                                    // Common indicators: unexpected EOF, unterminated string, etc.
                                    let error_str = e.to_string().to_lowercase();
                                    if line.starts_with("data: ")
                                        && (error_str.contains("eof") ||
                                        error_str.contains("unterminated") ||
                                        error_str.contains("unexpected end") ||
                                        error_str.contains("trailing") ||
                                        // Also check if the data doesn't end with a proper JSON terminator
                                        (!data.trim_end().ends_with('}') && !data.trim_end().ends_with(']')))
                                    {
                                        // This looks like an incomplete data line, save it for the next chunk
                                        debug!("Detected incomplete data line (len={}), buffering for next chunk", line.len());
                                        incomplete_data_line = line.clone();
                                        // Continue to next iteration without processing
                                        continue;
                                    } else {
                                        // This is a real parse error, not due to line splitting
                                        debug!("Failed to parse Databricks stream chunk JSON: {} - Data length: {}", e, data.len());
                                        // For debugging large payloads, log a sample
                                        if data.len() > 1000 {
                                            debug!(
                                                "JSON parse error - data sample: {}",
                                                &data[..std::cmp::min(500, data.len())]
                                            );
                                        }
                                    }
                                    // Don't error out on parse failures, just continue
                                }
                            }
                        } else if line.starts_with("event: ") || line.starts_with("id: ") {
                            // Debug: Log non-data SSE lines (like event: or id:)
                            debug!("Non-data SSE line: {}", line);
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

        // If we have any incomplete data line at the end, try to process it
        if !incomplete_data_line.is_empty() {
            debug!(
                "Processing final incomplete data line (len={})",
                incomplete_data_line.len()
            );
            if let Some(data) = incomplete_data_line.strip_prefix("data: ") {
                // Try to parse it as-is, it might be complete
                if let Ok(_chunk) = serde_json::from_str::<DatabricksStreamChunk>(data) {
                    // Process the chunk (code would be duplicated from above, so in practice
                    // we'd extract this to a helper function)
                    debug!("Successfully parsed final incomplete data line");
                } else {
                    warn!("Failed to parse final incomplete data line");
                }
            }
        }

        // Send final chunk if we haven't already
        let final_tool_calls: Vec<ToolCall> = current_tool_calls
            .values()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| ToolCall {
                id: if id.is_empty() {
                    format!("tool_{}", name)
                } else {
                    id.clone()
                },
                tool: name.clone(),
                args: serde_json::from_str(args)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
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

    pub async fn fetch_supported_models(&mut self) -> Result<Option<Vec<String>>> {
        let token = self.auth.get_token().await?;

        let response = match self
            .client
            .get(format!("{}/api/2.0/serving-endpoints", self.host))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Failed to fetch Databricks models: {}", e);
                return Ok(None);
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            if let Ok(error_text) = response.text().await {
                warn!(
                    "Failed to fetch Databricks models: {} - {}",
                    status, error_text
                );
            } else {
                warn!("Failed to fetch Databricks models: {}", status);
            }
            return Ok(None);
        }

        let json: serde_json::Value = match response.json().await {
            Ok(json) => json,
            Err(e) => {
                warn!("Failed to parse Databricks API response: {}", e);
                return Ok(None);
            }
        };

        let endpoints = match json.get("endpoints").and_then(|v| v.as_array()) {
            Some(endpoints) => endpoints,
            None => {
                warn!("Unexpected response format from Databricks API: missing 'endpoints' array");
                return Ok(None);
            }
        };

        let models: Vec<String> = endpoints
            .iter()
            .filter_map(|endpoint| {
                endpoint
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|name| name.to_string())
            })
            .collect();

        if models.is_empty() {
            debug!("No serving endpoints found in Databricks workspace");
            Ok(None)
        } else {
            debug!(
                "Found {} serving endpoints in Databricks workspace",
                models.len()
            );
            Ok(Some(models))
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for DatabricksProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing Databricks completion request with {} messages",
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
        )?;

        debug!(
            "Sending request to Databricks API: model={}, max_tokens={}, temperature={}",
            self.model, request_body.max_tokens, request_body.temperature
        );

        // Debug: Log the full request body when tools are present
        if request.tools.is_some() {
            debug!(
                "Full request body with tools: {}",
                serde_json::to_string_pretty(&request_body)
                    .unwrap_or_else(|_| "Failed to serialize".to_string())
            );
        }

        let mut provider_clone = self.clone();
        let mut response = provider_clone
            .create_request_builder(false)
            .await?
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send request to Databricks API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            // Check if this is a 403 Invalid Token error that we can retry with token refresh
            if status == reqwest::StatusCode::FORBIDDEN
                && (error_text.contains("Invalid Token") || error_text.contains("invalid_token"))
            {
                info!("Received 403 Invalid Token error, attempting to refresh OAuth token");

                // Try to refresh the token if we're using OAuth
                if let DatabricksAuth::OAuth { .. } = &provider_clone.auth {
                    // Clear any cached token to force a refresh
                    provider_clone.auth.clear_cached_token();

                    // Try to get a new token (will attempt refresh or new OAuth flow)
                    match provider_clone.auth.get_token().await {
                        Ok(_new_token) => {
                            info!("Successfully refreshed OAuth token, retrying request");

                            // Retry the request with the new token
                            response = provider_clone
                                .create_request_builder(false)
                                .await?
                                .json(&request_body)
                                .send()
                                .await
                                .map_err(|e| anyhow!("Failed to send request to Databricks API after token refresh: {}", e))?;

                            let retry_status = response.status();
                            if !retry_status.is_success() {
                                let retry_error_text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unknown error".to_string());
                                return Err(anyhow!(
                                    "Databricks API error {} after token refresh: {}",
                                    retry_status,
                                    retry_error_text
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Failed to refresh OAuth token: {}. Original error: {}",
                                e,
                                error_text
                            ));
                        }
                    }
                } else {
                    return Err(anyhow!("Databricks API error {}: {}", status, error_text));
                }
            } else {
                return Err(anyhow!("Databricks API error {}: {}", status, error_text));
            }
        }

        let response_text = response.text().await?;
        debug!("Raw Databricks API response: {}", response_text);

        let databricks_response: DatabricksResponse = serde_json::from_str(&response_text)
            .map_err(|e| {
                anyhow!(
                    "Failed to parse Databricks response: {} - Response: {}",
                    e,
                    response_text
                )
            })?;

        // Debug: Log the parsed response structure
        debug!("Parsed Databricks response: {:#?}", databricks_response);

        // Extract content from the first choice
        let content = databricks_response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .cloned()
            .unwrap_or_default();

        // Check if there are tool calls in the response
        if let Some(first_choice) = databricks_response.choices.first() {
            if let Some(tool_calls) = &first_choice.message.tool_calls {
                debug!(
                    "Found {} tool calls in Databricks response",
                    tool_calls.len()
                );
                for (i, tool_call) in tool_calls.iter().enumerate() {
                    debug!(
                        "Tool call {}: {} with args: {}",
                        i, tool_call.function.name, tool_call.function.arguments
                    );
                }

                // For now, we'll return the content as-is since g3 handles tool calls via streaming
                // In the future, we might need to convert these to the internal format
            }
        }

        let usage = Usage {
            prompt_tokens: databricks_response.usage.prompt_tokens,
            completion_tokens: databricks_response.usage.completion_tokens,
            total_tokens: databricks_response.usage.total_tokens,
        };

        debug!(
            "Databricks completion successful: {} tokens generated",
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
            "Processing Databricks streaming request with {} messages",
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
        )?;

        debug!(
            "Sending streaming request to Databricks API: model={}, max_tokens={}, temperature={}",
            self.model, request_body.max_tokens, request_body.temperature
        );

        // Debug: Log the full request body
        debug!(
            "Full request body: {}",
            serde_json::to_string_pretty(&request_body)
                .unwrap_or_else(|_| "Failed to serialize".to_string())
        );

        let mut provider_clone = self.clone();
        let mut response = provider_clone
            .create_request_builder(true)
            .await?
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send streaming request to Databricks API: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            // Check if this is a 403 Invalid Token error that we can retry with token refresh
            if status == reqwest::StatusCode::FORBIDDEN
                && (error_text.contains("Invalid Token") || error_text.contains("invalid_token"))
            {
                info!("Received 403 Invalid Token error, attempting to refresh OAuth token");

                // Try to refresh the token if we're using OAuth
                if let DatabricksAuth::OAuth { .. } = &provider_clone.auth {
                    // Clear any cached token to force a refresh
                    provider_clone.auth.clear_cached_token();

                    // Try to get a new token (will attempt refresh or new OAuth flow)
                    match provider_clone.auth.get_token().await {
                        Ok(_new_token) => {
                            info!("Successfully refreshed OAuth token, retrying streaming request");

                            // Retry the request with the new token
                            response = provider_clone
                                .create_request_builder(true)
                                .await?
                                .json(&request_body)
                                .send()
                                .await
                                .map_err(|e| anyhow!("Failed to send streaming request to Databricks API after token refresh: {}", e))?;

                            let retry_status = response.status();
                            if !retry_status.is_success() {
                                let retry_error_text = response
                                    .text()
                                    .await
                                    .unwrap_or_else(|_| "Unknown error".to_string());
                                return Err(anyhow!(
                                    "Databricks API error {} after token refresh: {}",
                                    retry_status,
                                    retry_error_text
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Failed to refresh OAuth token: {}. Original error: {}",
                                e,
                                error_text
                            ));
                        }
                    }
                } else {
                    return Err(anyhow!("Databricks API error {}: {}", status, error_text));
                }
            } else {
                return Err(anyhow!("Databricks API error {}: {}", status, error_text));
            }
        }

        let stream = response.bytes_stream();
        let (tx, rx) = mpsc::channel(100);

        // Spawn task to process the stream
        let provider = self.clone();
        tokio::spawn(async move {
            provider.parse_streaming_response(stream, tx).await;
        });

        Ok(ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        "databricks"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // Databricks Foundation Models support native tool calling
        // This includes Claude, Llama, DBRX, and most other models on the platform
        true
    }
}

// Databricks API request/response structures

#[derive(Debug, Serialize)]
struct DatabricksRequest {
    messages: Vec<DatabricksMessage>,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<DatabricksTool>>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct DatabricksTool {
    r#type: String,
    function: DatabricksFunction,
}

#[derive(Debug, Serialize)]
struct DatabricksFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct DatabricksMessage {
    role: String,
    content: Option<String>, // Make content optional since tool calls might not have content
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<DatabricksToolCall>>, // Add tool_calls field for responses
}

#[derive(Debug, Serialize, Deserialize)]
struct DatabricksToolCall {
    id: String,
    r#type: String,
    function: DatabricksToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct DatabricksToolCallFunction {
    name: String,
    arguments: String, // This will be a JSON string that needs parsing
}

#[derive(Debug, Deserialize)]
struct DatabricksResponse {
    choices: Vec<DatabricksChoice>,
    usage: DatabricksUsage,
}

#[derive(Debug, Deserialize)]
struct DatabricksChoice {
    message: DatabricksMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DatabricksUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// Streaming response structures

#[derive(Debug, Deserialize)]
struct DatabricksStreamChunk {
    choices: Option<Vec<DatabricksStreamChoice>>,
}

#[derive(Debug, Deserialize)]
struct DatabricksStreamChoice {
    delta: Option<DatabricksStreamDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DatabricksStreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<DatabricksStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DatabricksStreamToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: DatabricksStreamFunction,
}

#[derive(Debug, Deserialize)]
struct DatabricksStreamFunction {
    #[serde(default)]
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_conversion() {
        let provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "test-model".to_string(),
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
            Message {
                role: MessageRole::Assistant,
                content: "Hi there!".to_string(),
            },
        ];

        let databricks_messages = provider.convert_messages(&messages).unwrap();

        assert_eq!(databricks_messages.len(), 3);
        assert_eq!(databricks_messages[0].role, "system");
        assert_eq!(databricks_messages[1].role, "user");
        assert_eq!(databricks_messages[2].role, "assistant");
    }

    #[test]
    fn test_request_body_creation() {
        let provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "databricks-claude-sonnet-4".to_string(),
            Some(1000),
            Some(0.5),
        )
        .unwrap();

        let messages = vec![Message {
            role: MessageRole::User,
            content: "Test message".to_string(),
        }];

        let request_body = provider
            .create_request_body(&messages, None, false, 1000, 0.5)
            .unwrap();

        assert_eq!(request_body.max_tokens, 1000);
        assert_eq!(request_body.temperature, 0.5);
        assert!(!request_body.stream);
        assert_eq!(request_body.messages.len(), 1);
        assert!(request_body.tools.is_none());
    }

    #[test]
    fn test_tool_conversion() {
        let provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "test-model".to_string(),
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

        let databricks_tools = provider.convert_tools(&tools);

        assert_eq!(databricks_tools.len(), 1);
        assert_eq!(databricks_tools[0].r#type, "function");
        assert_eq!(databricks_tools[0].function.name, "get_weather");
        assert_eq!(
            databricks_tools[0].function.description,
            "Get the current weather"
        );
    }

    #[test]
    fn test_has_native_tool_calling() {
        let claude_provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "databricks-claude-sonnet-4".to_string(),
            None,
            None,
        )
        .unwrap();

        let llama_provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "databricks-meta-llama-3-3-70b-instruct".to_string(),
            None,
            None,
        )
        .unwrap();

        let dbrx_provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "databricks-dbrx-instruct".to_string(),
            None,
            None,
        )
        .unwrap();

        assert!(claude_provider.has_native_tool_calling());
        assert!(llama_provider.has_native_tool_calling());
        assert!(dbrx_provider.has_native_tool_calling());
    }
}
