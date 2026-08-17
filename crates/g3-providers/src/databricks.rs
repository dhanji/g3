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

use anyhow::{anyhow, Result};
use bytes::Bytes;
use crate::streaming::{decode_utf8_streaming, is_incomplete_json_error, make_final_chunk};
use futures_util::stream::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, warn};
use std::collections::HashMap;

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

// ─────────────────────────────────────────────────────────────────────────────
// Streaming helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Accumulated state for a single tool call being streamed in chunks.
#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    args: String,
}

impl ToolCallAccumulator {
    /// Update accumulator with a streaming delta.
    fn apply_delta(&mut self, delta: &DatabricksStreamToolCall) {
        if let Some(ref id) = delta.id {
            self.id = id.clone();
        }
        if !delta.function.name.is_empty() {
            self.name = delta.function.name.clone();
        }
        self.args.push_str(&delta.function.arguments);
    }

    /// Convert to final ToolCall if valid (has a name).
    fn into_tool_call(self) -> Option<ToolCall> {
        if self.name.is_empty() {
            return None;
        }
        let id = if self.id.is_empty() {
            format!("tool_{}", self.name)
        } else {
            self.id
        };
        let args = serde_json::from_str(&self.args)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
        Some(ToolCall { id, tool: self.name, args })
    }
}

/// Convert accumulated tool calls map to final Vec<ToolCall>.
fn finalize_tool_calls(accumulators: HashMap<usize, ToolCallAccumulator>) -> Vec<ToolCall> {
    accumulators
        .into_values()
        .filter_map(|acc| acc.into_tool_call())
        .collect()
}

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
    name: String,
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

        debug!(
            "Initialized Databricks provider with model: {} on host: {}",
            model, host
        );

        Ok(Self {
            client,
            name: "databricks".to_string(),
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::token(token),
            model,
            max_tokens: max_tokens.unwrap_or(32000),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    /// Create a DatabricksProvider with token auth and a custom name (e.g., "databricks.default")
    pub fn from_token_with_name(
        name: String,
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

        debug!("Initialized Databricks provider '{}' with model: {} on host: {}", name, model, host);

        Ok(Self {
            client,
            name,
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::token(token),
            model,
            max_tokens: max_tokens.unwrap_or(32000),
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

        debug!(
            "Initialized Databricks provider with OAuth for model: {} on host: {}",
            model, host
        );

        Ok(Self {
            client,
            name: "databricks".to_string(),
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::oauth(host.clone()),
            model,
            max_tokens: max_tokens.unwrap_or(32000),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    /// Create a DatabricksProvider with OAuth auth and a custom name (e.g., "databricks.default")
    pub async fn from_oauth_with_name(
        name: String,
        host: String,
        model: String,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        debug!("Initialized Databricks provider '{}' with OAuth for model: {} on host: {}", name, model, host);

        Ok(Self {
            client,
            name,
            host: host.trim_end_matches('/').to_string(),
            auth: DatabricksAuth::oauth(host.clone()),
            model,
            max_tokens: max_tokens.unwrap_or(32000),
            temperature: temperature.unwrap_or(0.1),
        })
    }

    async fn create_request_builder(&mut self, streaming: bool) -> Result<RequestBuilder> {
        let token = self.auth.get_token().await?;

        let mut builder = self
            .client
            .post(format!(
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

            // Always use simple string format (Databricks doesn't support cache_control)
            let content = serde_json::Value::String(message.content.clone());

            databricks_messages.push(DatabricksMessage {
                role: role.to_string(),
                content: Some(content),
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
        let mut tool_calls: HashMap<usize, ToolCallAccumulator> = HashMap::new();
        let mut incomplete_data_line = String::new();
        let mut chunk_count = 0;
        let mut byte_buffer = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            // Handle stream errors
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    error!("Stream error at chunk {}: {}", chunk_count, e);
                    let is_connection_error = e.to_string().contains("unexpected EOF")
                        || e.to_string().contains("connection");
                    if is_connection_error {
                        warn!("Connection terminated unexpectedly, treating as end of stream");
                        break;
                    }
                    let _ = tx.send(Err(anyhow!("Stream error: {}", e))).await;
                    return None;
                }
            };

            chunk_count += 1;
            byte_buffer.extend_from_slice(&chunk);

            // Decode UTF-8, handling incomplete sequences
            let Some(chunk_str) = decode_utf8_streaming(&mut byte_buffer) else {
                continue;
            };
            buffer.push_str(&chunk_str);

            // Process complete lines
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer.drain(..line_end + 1);

                if line.is_empty() {
                    continue;
                }

                // Reassemble lines split across chunks
                let line = if !incomplete_data_line.is_empty() {
                    let complete = format!("{}{}", incomplete_data_line, line);
                    incomplete_data_line.clear();
                    complete
                } else {
                    line
                };

                // Parse SSE data lines
                let Some(data) = line.strip_prefix("data: ") else {
                    if line.starts_with("event: ") || line.starts_with("id: ") {
                        debug!("SSE control line: {}", line);
                    }
                    continue;
                };

                // Stream completion marker
                if data == "[DONE]" {
                    debug!("Received stream completion marker");
                    let final_calls = finalize_tool_calls(tool_calls);
                    let _ = tx.send(Ok(make_final_chunk(final_calls, None))).await;
                    return None;
                }

                // Parse JSON payload
                let parsed = match serde_json::from_str::<DatabricksStreamChunk>(data) {
                    Ok(c) => c,
                    Err(e) => {
                        if is_incomplete_json_error(&e, data) {
                            debug!("Incomplete JSON, buffering for next chunk");
                            incomplete_data_line = line;
                        } else {
                            debug!("JSON parse error: {}", e);
                        }
                        continue;
                    }
                };

                // Process choices from the chunk
                let Some(choices) = parsed.choices else { continue };
                for choice in choices {
                    // Handle delta content
                    if let Some(delta) = &choice.delta {
                        // Text content
                        if let Some(ref content) = delta.content {
                            let text_chunk = CompletionChunk {
                                content: content.clone(),
                                finished: false,
                                usage: None,
                                tool_calls: None,
                                stop_reason: None,
                                tool_call_streaming: None,
                                upstream_ping: false,
                            };
                            if tx.send(Ok(text_chunk)).await.is_err() {
                                debug!("Receiver dropped");
                                return None;
                            }
                        }

                        // Tool call deltas
                        if let Some(ref deltas) = delta.tool_calls {
                            for tc_delta in deltas {
                                let idx = tc_delta.index.unwrap_or(0);
                                tool_calls
                                    .entry(idx)
                                    .or_default()
                                    .apply_delta(tc_delta);
                            }
                        }
                    }

                    // Choice finished
                    if choice.finish_reason.is_some() {
                        debug!("Choice finished: {:?}", choice.finish_reason);
                        let final_calls = finalize_tool_calls(std::mem::take(&mut tool_calls));
                        let _ = tx.send(Ok(make_final_chunk(final_calls, None))).await;
                        return None;
                    }
                }
            }
        }

        debug!("Stream ended after {} chunks", chunk_count);
        let final_calls = finalize_tool_calls(tool_calls);
        let _ = tx.send(Ok(make_final_chunk(final_calls, None))).await;
        None
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
                debug!("Received 403 Invalid Token error, attempting to refresh OAuth token");

                // Try to refresh the token if we're using OAuth
                if let DatabricksAuth::OAuth { .. } = &provider_clone.auth {
                    // Clear any cached token to force a refresh
                    provider_clone.auth.clear_cached_token();

                    // Try to get a new token (will attempt refresh or new OAuth flow)
                    match provider_clone.auth.get_token().await {
                        Ok(_new_token) => {
                            debug!("Successfully refreshed OAuth token, retrying request");

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
            .and_then(|choice| {
                choice.message.content.as_ref().map(|c| {
                    // Handle both string and array formats
                    if let Some(s) = c.as_str() {
                        s.to_string()
                    } else if let Some(arr) = c.as_array() {
                        // Extract text from content blocks
                        arr.iter()
                            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    } else {
                        String::new()
                    }
                })
            })
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
            cache_creation_tokens: 0, // Databricks doesn't support prompt caching
            cache_read_tokens: 0,
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

        // Debug: Log tool count
        if let Some(ref tools) = request.tools {
            debug!("Request has {} tools", tools.len());
            for tool in tools.iter().take(5) {
                debug!("  Tool: {}", tool.name);
            }
        }

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
                debug!("Received 403 Invalid Token error, attempting to refresh OAuth token");

                // Try to refresh the token if we're using OAuth
                if let DatabricksAuth::OAuth { .. } = &provider_clone.auth {
                    // Clear any cached token to force a refresh
                    provider_clone.auth.clear_cached_token();

                    // Try to get a new token (will attempt refresh or new OAuth flow)
                    match provider_clone.auth.get_token().await {
                        Ok(_new_token) => {
                            debug!("Successfully refreshed OAuth token, retrying streaming request");

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
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn has_native_tool_calling(&self) -> bool {
        // Databricks Foundation Models support native tool calling
        // This includes Claude, Llama, DBRX, and most other models on the platform
        true
    }

    fn supports_cache_control(&self) -> bool {
        false
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn temperature(&self) -> f32 {
        self.temperature
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
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>, // Can be string or array of content blocks
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
            Message::new(
                MessageRole::System,
                "You are a helpful assistant.".to_string(),
            ),
            Message::new(MessageRole::User, "Hello!".to_string()),
            Message::new(MessageRole::Assistant, "Hi there!".to_string()),
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

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];

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

    #[test]
    fn test_cache_control_serialization() {
        let provider = DatabricksProvider::from_token(
            "https://test.databricks.com".to_string(),
            "test-token".to_string(),
            "databricks-claude-sonnet-4".to_string(),
            None,
            None,
        )
        .unwrap();

        // Test message WITHOUT cache_control
        let messages_without = vec![Message::new(MessageRole::User, "Hello".to_string())];
        let databricks_messages_without = provider.convert_messages(&messages_without).unwrap();
        let json_without = serde_json::to_string(&databricks_messages_without).unwrap();

        println!("JSON without cache_control: {}", json_without);
        assert!(
            !json_without.contains("cache_control"),
            "JSON should not contain 'cache_control' field when not configured"
        );

        // Test message WITH cache_control - should still NOT include it (Databricks doesn't support it)
        let messages_with = vec![Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            crate::CacheControl::ephemeral(),
        )];
        let databricks_messages_with = provider.convert_messages(&messages_with).unwrap();
        let json_with = serde_json::to_string(&databricks_messages_with).unwrap();

        println!("JSON with cache_control: {}", json_with);
        assert!(
            !json_with.contains("cache_control"),
            "JSON should NOT contain 'cache_control' field - Databricks doesn't support it"
        );
    }

    #[test]
    fn test_databricks_does_not_support_cache_control() {
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

        assert!(
            !claude_provider.supports_cache_control(),
            "Databricks should not support cache_control even for Claude models"
        );
        assert!(
            !llama_provider.supports_cache_control(),
            "Databricks should not support cache_control for Llama models"
        );
    }
}
