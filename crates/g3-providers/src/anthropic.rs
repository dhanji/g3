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
//!         Some("claude-3-5-sonnet-20241022".to_string()),
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
//!         temperature: None, // Note: Anthropic API ignores temperature; field
//!                            // retained on CompletionRequest for other providers.
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
//!         None,
//!         None,
//!         None,
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
//!         temperature: None, // Note: Anthropic API ignores temperature.
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
    streaming::{
        decode_utf8_streaming, make_final_chunk, make_final_chunk_with_reason, make_text_chunk,
        make_tool_chunk, make_tool_streaming_active, make_tool_streaming_hint,
        make_upstream_ping_chunk,
    },
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The Messages endpoint, overridable via `ANTHROPIC_BASE_URL` for local
/// observation.
///
/// g3 reads `cache_read_input_tokens` off the wire and then discards it — it
/// reaches no log, no UI and no session file. That makes "is prompt caching
/// working?" unfalsifiable from inside: a cached run and an uncached run look
/// identical from here. Pointing this at a local proxy is the only way to see
/// the number that answers it.
///
/// Defaults to the real API, so an unset variable changes nothing.
fn anthropic_api_url() -> String {
    match std::env::var("ANTHROPIC_BASE_URL") {
        Ok(base) if !base.trim().is_empty() => {
            format!("{}/v1/messages", base.trim_end_matches('/'))
        }
        _ => "https://api.anthropic.com/v1/messages".to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    client: Client,
    name: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    cache_config: Option<String>,
    enable_1m_context: bool,
    thinking_budget_tokens: Option<u32>,
}

// ── SSE Stream State ────────────────────────────────────────────────────
// Mutable state threaded through Anthropic's SSE stream parser.
// Each `handle_*` method processes one event type and returns chunks to send.

struct StreamState {
    tool_calls: Vec<ToolCall>,
    partial_tool_json: String,
    usage: Option<Usage>,
    message_stopped: bool,
    stop_reason: Option<String>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            tool_calls: Vec::new(),
            partial_tool_json: String::new(),
            usage: None,
            message_stopped: false,
            stop_reason: None,
        }
    }

    fn handle_message_start(&mut self, event: &AnthropicStreamEvent) {
        if let Some(message) = &event.message {
            if let Some(u) = &message.usage {
                self.usage = Some(Usage {
                    prompt_tokens: u.input_tokens,
                    completion_tokens: u.output_tokens,
                    total_tokens: u.input_tokens + u.output_tokens,
                    cache_creation_tokens: u.cache_creation_input_tokens,
                    cache_read_tokens: u.cache_read_input_tokens,
                });
                debug!("Captured usage from message_start: {:?}", self.usage);
            }
        }
    }

    /// Returns chunks to send for a content_block_start event.
    fn handle_block_start(&mut self, event: AnthropicStreamEvent) -> Vec<Result<CompletionChunk>> {
        let Some(content_block) = event.content_block else { return vec![] };
        match content_block {
            AnthropicContent::ToolUse { id, name, input } => {
                debug!("Tool use block: id={}, name={}, input={:?}", id, name, input);
                let tool_call = ToolCall { id: id.clone(), tool: name.clone(), args: input.clone() };

                let has_complete_args = !input.is_null()
                    && input != serde_json::Value::Object(serde_json::Map::new());

                if has_complete_args {
                    debug!("Tool call has complete args, sending immediately");
                    vec![Ok(make_tool_chunk(vec![tool_call]))]
                } else {
                    debug!("Tool call has empty args, will accumulate from partial_json");
                    let hint = make_tool_streaming_hint(name);
                    self.tool_calls.push(tool_call);
                    self.partial_tool_json.clear();
                    vec![Ok(hint)]
                }
            }
            _ => {
                debug!("Non-tool content block: {:?}", content_block);
                vec![]
            }
        }
    }

    /// Returns chunks to send for a content_block_delta event.
    fn handle_block_delta(&mut self, event: AnthropicStreamEvent) -> Vec<Result<CompletionChunk>> {
        let Some(delta) = event.delta else { return vec![] };
        let mut chunks = Vec::new();
        if let Some(text) = delta.text {
            debug!("Text chunk (len {})", text.len());
            chunks.push(Ok(make_text_chunk(text)));
        }
        if let Some(json_fragment) = delta.partial_json {
            debug!("Partial JSON: {}", json_fragment);
            self.partial_tool_json.push_str(&json_fragment);
            chunks.push(Ok(make_tool_streaming_active()));
        }
        chunks
    }

    /// Returns chunks to send when a content block finishes.
    fn handle_block_stop(&mut self) -> Vec<Result<CompletionChunk>> {
        // Finalize accumulated partial JSON into the last tool call's args
        if !self.tool_calls.is_empty() && !self.partial_tool_json.is_empty() {
            debug!("Parsing complete tool JSON: {}", self.partial_tool_json);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&self.partial_tool_json) {
                if let Some(last) = self.tool_calls.last_mut() {
                    last.args = parsed;
                    debug!("Updated tool call with complete args: {:?}", last);
                }
            } else {
                debug!("Failed to parse accumulated JSON: {}", self.partial_tool_json);
            }
            self.partial_tool_json.clear();
        }

        if self.tool_calls.is_empty() {
            return vec![];
        }
        let chunk = make_tool_chunk(self.tool_calls.clone());
        self.tool_calls.clear();
        vec![Ok(chunk)]
    }

    fn handle_message_delta(&mut self, event: &AnthropicStreamEvent) {
        if let Some(delta) = &event.delta {
            if let Some(reason) = &delta.stop_reason {
                debug!("Received stop_reason: {}", reason);
                self.stop_reason = Some(reason.clone());
            }
        }
    }

    fn handle_message_stop(&mut self) -> Vec<Result<CompletionChunk>> {
        debug!("Received message stop event");
        self.message_stopped = true;
        let chunk = make_final_chunk_with_reason(
            self.tool_calls.clone(),
            self.usage.clone(),
            self.stop_reason.clone(),
        );
        vec![Ok(chunk)]
    }
}

impl AnthropicProvider {
    /// Create a new AnthropicProvider.
    ///
    /// # Note on `temperature`
    /// The `temperature` parameter is **accepted but never sent on the wire**.
    /// Anthropic's API rejects the `temperature` field for newer models
    /// (e.g. extended-thinking / reasoning models), so it is stripped from
    /// `AnthropicRequest` entirely. The value is still stored on the provider
    /// and exposed via [`LLMProvider::temperature`] for callers that read the
    /// configured value for other purposes (e.g. seeding requests against a
    /// different provider). See `temperature()` impl for details. To re-add
    /// temperature to the wire format, restore the field on `AnthropicRequest`
    /// and reinstate the regression test guarding against it.
    pub fn new(
        api_key: String,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
        cache_config: Option<String>,
        enable_1m_context: Option<bool>,
        thinking_budget_tokens: Option<u32>,
    ) -> Result<Self> {
        Self::new_with_name("anthropic".to_string(), api_key, model, max_tokens, temperature, cache_config, enable_1m_context, thinking_budget_tokens)
    }

    /// Create a new AnthropicProvider with a custom name (e.g., "anthropic.default")
    pub fn new_with_name(
        name: String,
        api_key: String,
        model: Option<String>,
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

        debug!(
            "Initialized Anthropic provider '{}' with model: {}",
            name, model
        );

        Ok(Self {
            client,
            name,
            api_key,
            model,
            max_tokens: max_tokens.unwrap_or(32768),
            temperature: temperature.unwrap_or(0.1),
            cache_config,
            enable_1m_context: enable_1m_context.unwrap_or(false),
            thinking_budget_tokens,
        })
    }

    fn create_request_builder(&self, streaming: bool) -> RequestBuilder {
        let mut builder = self
            .client
            .post(anthropic_api_url())
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

    // Anthropic uses the same CacheControl format — no conversion needed, just clone at call sites.

    /// The configured cache policy, parsed from the provider's `cache_config`
    /// string, or `None` when caching is off.
    ///
    /// The accepted spellings match `g3-core`'s `parse_cache_control()` exactly.
    /// An unrecognised value yields `None` — caching simply does not engage,
    /// which is the same behaviour as leaving it unset. That is deliberate: a
    /// typo in a cost optimisation must never take the agent down.
    fn parsed_cache_control(&self) -> Option<crate::CacheControl> {
        match self.cache_config.as_deref()?.trim() {
            "ephemeral" => Some(crate::CacheControl::ephemeral()),
            "5minute" => Some(crate::CacheControl::five_minute()),
            "1hour" => Some(crate::CacheControl::one_hour()),
            other => {
                tracing::warn!(
                    "Invalid cache_config '{}' (expected: ephemeral, 5minute, 1hour). \
                     Prompt caching is DISABLED for this provider.",
                    other
                );
                None
            }
        }
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
                    let mut content_blocks: Vec<AnthropicContent> = Vec::new();

                    // Check if this is a tool result message
                    if let Some(ref tool_use_id) = message.tool_result_id {
                        // If images are attached, use structured content (array of blocks)
                        // inside the tool_result. Anthropic API rejects top-level Image
                        // blocks mixed with ToolResult blocks in the same user message.
                        let content = if message.images.is_empty() {
                            ToolResultContent::Text(message.content.clone())
                        } else {
                            let mut blocks: Vec<ToolResultBlock> = Vec::new();
                            for image in &message.images {
                                blocks.push(ToolResultBlock::Image {
                                    source: AnthropicImageSource {
                                        source_type: "base64".to_string(),
                                        media_type: image.media_type.clone(),
                                        data: image.data.clone(),
                                    },
                                });
                            }
                            blocks.push(ToolResultBlock::Text {
                                text: message.content.clone(),
                            });
                            ToolResultContent::Blocks(blocks)
                        };
                        content_blocks.push(AnthropicContent::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content,
                            cache_control: message
                                .cache_control
                                .as_ref()
                                .map(|cc| cc.clone()),
                        });
                    } else {
                        // Regular user message: images as top-level blocks, then text
                        for image in &message.images {
                            content_blocks.push(AnthropicContent::Image {
                                source: AnthropicImageSource {
                                    source_type: "base64".to_string(),
                                    media_type: image.media_type.clone(),
                                    data: image.data.clone(),
                                },
                            });
                        }
                        content_blocks.push(AnthropicContent::Text {
                            text: message.content.clone(),
                            cache_control: message
                                .cache_control
                                .as_ref()
                                .map(|cc| cc.clone()),
                        });
                    }

                    anthropic_messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: content_blocks,
                    });
                }
                MessageRole::Assistant => {
                    let mut content_blocks: Vec<AnthropicContent> = Vec::new();

                    // Add text content if non-empty
                    if !message.content.trim().is_empty() {
                        content_blocks.push(AnthropicContent::Text {
                            text: message.content.clone(),
                            cache_control: message
                                .cache_control
                                .as_ref()
                                .map(|cc| cc.clone()),
                        });
                    }

                    // Add tool_use blocks for any structured tool calls
                    for tc in &message.tool_calls {
                        content_blocks.push(AnthropicContent::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.input.clone(),
                        });
                    }

                    // Ensure we have at least one content block
                    if content_blocks.is_empty() {
                        content_blocks.push(AnthropicContent::Text {
                            text: message.content.clone(),
                            cache_control: None,
                        });
                    }

                    anthropic_messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: content_blocks,
                    });
                }
            }
        }

        // Defense-in-depth: strip orphaned tool_use blocks that have no matching tool_result
        Self::strip_orphaned_tool_use(&mut anthropic_messages);

        Ok((system_message, anthropic_messages))
    }

    /// Strip orphaned tool_use blocks from assistant messages that have no matching
    /// tool_result in the immediately following user message.
    ///
    /// Anthropic API requires: "Each tool_use block must have a corresponding tool_result
    /// block in the next message." This can happen after context compaction when the
    /// last assistant message had tool_calls but the tool_result was summarized away.
    fn strip_orphaned_tool_use(messages: &mut Vec<AnthropicMessage>) {
        // Collect tool_result IDs from each user message, indexed by position
        let tool_result_ids_by_pos: Vec<Option<Vec<String>>> = messages
            .iter()
            .map(|msg| {
                if msg.role == "user" {
                    let ids: Vec<String> = msg
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            AnthropicContent::ToolResult { tool_use_id, .. } => {
                                Some(tool_use_id.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    if ids.is_empty() { None } else { Some(ids) }
                } else {
                    None
                }
            })
            .collect();

        for i in 0..messages.len() {
            if messages[i].role != "assistant" {
                continue;
            }
            let has_tool_use = messages[i].content.iter().any(|c| matches!(c, AnthropicContent::ToolUse { .. }));
            if !has_tool_use {
                continue;
            }

            // Check if next message is a user message with tool_result blocks
            let next_has_results = i + 1 < messages.len()
                && tool_result_ids_by_pos.get(i + 1).and_then(|v| v.as_ref()).is_some();

            if !next_has_results {
                let tool_use_ids: Vec<String> = messages[i]
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AnthropicContent::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                tracing::warn!(
                    "Stripping {} orphaned tool_use block(s) from assistant message {}: {:?}",
                    tool_use_ids.len(), i, tool_use_ids
                );
                messages[i].content.retain(|c| !matches!(c, AnthropicContent::ToolUse { .. }));

                // If stripping left the message empty, add placeholder text
                if messages[i].content.is_empty() {
                    messages[i].content.push(AnthropicContent::Text {
                        text: "(continued)".to_string(),
                        cache_control: None,
                    });
                }
            }
        }
    }

    fn create_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        streaming: bool,
        max_tokens: u32,
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

        // Wrap the system prompt in a cached content block when caching is
        // configured. This prefix (agent prompt + skills + memory + facts) is
        // byte-identical on every call of a session and is the largest reusable
        // span in the request, so it is the highest-value breakpoint available
        // — and, being a fixed prefix, also the safest.
        let system = system.map(|text| match self.parsed_cache_control() {
            // An empty prompt gets no block: a zero-length cached segment is
            // below the API's minimum cacheable length, and caching nothing
            // spends one of only four breakpoints for no gain.
            Some(cc) if !text.trim().is_empty() => {
                SystemPrompt::Blocks(vec![AnthropicContent::Text {
                    text,
                    cache_control: Some(cc),
                }])
            }
            _ => SystemPrompt::Text(text),
        });

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
        let mut state = StreamState::new();
        let mut line_buffer = String::new();
        let mut byte_buffer: Vec<u8> = Vec::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    byte_buffer.extend_from_slice(&chunk);
                    let Some(chunk_str) = decode_utf8_streaming(&mut byte_buffer) else {
                        continue;
                    };
                    line_buffer.push_str(&chunk_str);

                    while let Some(line_end) = line_buffer.find('\n') {
                        let line = line_buffer[..line_end].trim().to_string();
                        line_buffer.drain(..line_end + 1);

                        if line.is_empty() || state.message_stopped {
                            if state.message_stopped && !line.is_empty() {
                                debug!("Skipping event after message_stop: {}", line);
                            }
                            continue;
                        }

                        let Some(data) = line.strip_prefix("data: ") else { continue };

                        // Stream completion marker
                        if data == "[DONE]" {
                            debug!("Received stream completion marker");
                            let final_chunk = make_final_chunk(state.tool_calls.clone(), state.usage.clone());
                            let _ = tx.send(Ok(final_chunk)).await;
                            return state.usage;
                        }

                        debug!("Raw Claude API JSON: {}", data);

                        let event = match serde_json::from_str::<AnthropicStreamEvent>(data) {
                            Ok(e) => e,
                            Err(e) => {
                                debug!("Failed to parse stream event: {} - Data: {}", e, data);
                                continue;
                            }
                        };

                        debug!("Parsed event type: {}", event.event_type);

                        // Dispatch to per-event handlers; collect chunks to send
                        let chunks: Vec<Result<CompletionChunk>> = match event.event_type.as_str() {
                            "message_start" => { state.handle_message_start(&event); vec![] }
                            "content_block_start" => state.handle_block_start(event),
                            "content_block_delta" => state.handle_block_delta(event),
                            "content_block_stop" => state.handle_block_stop(),
                            "message_delta" => { state.handle_message_delta(&event); vec![] }
                            "message_stop" => state.handle_message_stop(),
                            // The upstream's own keep-alive. Anthropic sends this
                            // every 30.0s while the model is thinking (measured
                            // 2026-08-17: 3 consecutive intervals of exactly
                            // 30.00s across a 129.5s opus-5 request whose first
                            // text_delta landed at 85.4s).
                            //
                            // It used to fall into the `_ =>` catch-all below and
                            // vanish, which made the "including pings" comment on
                            // notify_sse_received() false: no chunk was produced,
                            // so nothing downstream ever learned the upstream was
                            // alive. That silence is what let a healthy 141.3s
                            // post-tool gap look identical to a wedged process.
                            "ping" => vec![Ok(make_upstream_ping_chunk())],
                            "error" => {
                                if let Some(error) = event.error {
                                    error!("Anthropic API error: {:?}", error);
                                    let _ = tx.send(Err(anyhow!("Anthropic API error: {:?}", error))).await;
                                    break;
                                }
                                vec![]
                            }
                            _ => { debug!("Ignoring event type: {}", event.event_type); vec![] }
                        };

                        // Send all chunks produced by the handler
                        for chunk in chunks {
                            if tx.send(chunk).await.is_err() {
                                debug!("Receiver dropped, stopping stream");
                                return state.usage;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Stream error: {}", e);
                    let _ = tx.send(Err(anyhow!("Stream error: {}", e))).await;
                    break;
                }
            }
        }

        let final_chunk = make_final_chunk(state.tool_calls, state.usage.clone());
        let _ = tx.send(Ok(final_chunk)).await;
        state.usage
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

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            false,
            max_tokens,
            request.disable_thinking,
        )?;

        debug!(
            "Sending request to Anthropic API: model={}, max_tokens={} (temperature omitted: not supported by Anthropic API)",
            request_body.model, request_body.max_tokens
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
            cache_creation_tokens: anthropic_response.usage.cache_creation_input_tokens,
            cache_read_tokens: anthropic_response.usage.cache_read_input_tokens,
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

        let request_body = self.create_request_body(
            &request.messages,
            request.tools.as_deref(),
            true,
            max_tokens,
            request.disable_thinking,
        )?;

        debug!(
            "Sending streaming request to Anthropic API: model={}, max_tokens={} (temperature omitted: not supported by Anthropic API)",
            request_body.model, request_body.max_tokens
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
        // Note: Anthropic's API no longer accepts a `temperature` field on the
        // wire (newer reasoning models reject it), so this value is NOT sent in
        // requests. It is retained for the LLMProvider trait contract — callers
        // such as g3-planner read it to seed `CompletionRequest.temperature` for
        // other providers that may share configuration.
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
        Self {
            thinking_type: "enabled".to_string(),
            budget_tokens,
        }
    }
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

/// The `system` field, in either shape the Anthropic API accepts.
///
/// It was a bare `String` until 2026-08-29, which meant the system prompt was
/// **structurally impossible to cache**: `cache_control` is a property of a
/// content *block*, and a JSON string has nowhere to hang one. For butler that
/// was the single largest recurring cost in the product — a ~34k-char prefix
/// (agent prompt + skill catalogue + workspace memory + facts) re-billed at the
/// full input rate on every one of ~66,000 API calls, because the one part of
/// the request guaranteed never to change was the one part that could not be
/// marked reusable.
///
/// `Text` keeps the old wire format exactly, so providers and tests that never
/// cache serialize byte-for-byte as before.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SystemPrompt {
    /// `"system": "..."` — unchanged legacy shape, used when caching is off.
    Text(String),
    /// `"system": [{"type":"text","text":"...","cache_control":{...}}]`
    Blocks(Vec<AnthropicContent>),
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
    Image { source: AnthropicImageSource },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<crate::CacheControl>,
    },
}

/// Content for a tool_result block. Can be either a simple string or an array
/// of content blocks (text + images). The Anthropic API accepts both forms.
/// We use the array form when images are present (e.g., from read_image).
#[derive(Debug, Clone)]
enum ToolResultContent {
    /// Simple text content: serializes as `"content": "text"`
    Text(String),
    /// Structured content blocks: serializes as `"content": [{"type": "image", ...}, {"type": "text", ...}]`
    Blocks(Vec<ToolResultBlock>),
}

/// A content block inside a tool_result's content array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ToolResultBlock {
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
    #[serde(rename = "text")]
    Text { text: String },
}

impl Serialize for ToolResultContent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolResultContent::Text(s) => serializer.serialize_str(s),
            ToolResultContent::Blocks(blocks) => blocks.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolResultContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // For deserialization, we only need to handle the string case (API responses don't
        // send tool_result back to us). But handle both for completeness.
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(ToolResultContent::Text(s)),
            serde_json::Value::Array(_) => {
                let blocks: Vec<ToolResultBlock> = serde_json::from_value(value)
                    .map_err(serde::de::Error::custom)?;
                Ok(ToolResultContent::Blocks(blocks))
            }
            _ => Ok(ToolResultContent::Text(value.to_string())),
        }
    }
}

/// Image source for Anthropic API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String, // Always "base64"
    media_type: String, // e.g., "image/png", "image/jpeg"
    data: String,       // Base64-encoded image data
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
    /// Tokens written to cache when creating a new cache entry
    #[serde(default)]
    cache_creation_input_tokens: u32,
    /// Tokens retrieved from cache (cache hit)
    #[serde(default)]
    cache_read_input_tokens: u32,
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
    stop_reason: Option<String>,
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
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
                .unwrap();

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
            Some(1000),
            Some(0.5),
            None,
            None,
            None,
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];

        let request_body = provider
            .create_request_body(&messages, None, false, 1000, false)
            .unwrap();

        assert_eq!(request_body.model, "claude-3-haiku-20240307");
        assert_eq!(request_body.max_tokens, 1000);
        assert!(!request_body.stream);
        assert_eq!(request_body.messages.len(), 1);
        assert!(request_body.tools.is_none());
    }

    /// Regression test: Anthropic's API rejects the `temperature` field for
    /// newer models (e.g. extended-thinking / reasoning models). The
    /// serialized request body MUST NOT contain a `temperature` key, for both
    /// streaming and non-streaming requests. If this test fails, someone
    /// likely re-added `temperature` to `AnthropicRequest`.
    #[test]
    fn test_request_body_omits_temperature_field() {
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
            Some(1000),
            Some(0.7), // Constructor accepts temperature but should NOT serialize it
            None,
            None,
            None,
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test".to_string())];

        // Non-streaming request body
        let request_non_stream = provider
            .create_request_body(&messages, None, false, 1000, false)
            .unwrap();
        let json_non_stream = serde_json::to_string(&request_non_stream).unwrap();
        assert!(
            !json_non_stream.contains("\"temperature\""),
            "Non-streaming AnthropicRequest JSON must NOT contain a 'temperature' \
             field (Anthropic API rejects it on newer models). Got: {}",
            json_non_stream
        );

        // Streaming request body
        let request_stream = provider
            .create_request_body(&messages, None, true, 1000, false)
            .unwrap();
        let json_stream = serde_json::to_string(&request_stream).unwrap();
        assert!(
            !json_stream.contains("\"temperature\""),
            "Streaming AnthropicRequest JSON must NOT contain a 'temperature' \
             field (Anthropic API rejects it on newer models). Got: {}",
            json_stream
        );
    }

    /// The system prompt must be CACHEABLE when caching is configured.
    ///
    /// Until 2026-08-29 `AnthropicRequest.system` was a bare `String`, so the
    /// largest fixed span in every request — agent prompt, skill catalogue,
    /// workspace memory, facts; ~34k chars for butler — could not carry a
    /// `cache_control` marker and was re-billed at the full input rate on
    /// every call. The bug was invisible: requests succeeded, and the only
    /// evidence was the invoice.
    ///
    /// These assertions are on the SERIALIZED JSON rather than the Rust type,
    /// because the wire format is the thing Anthropic bills on. A struct that
    /// looks right and serializes wrong costs real money.
    #[test]
    fn test_system_prompt_is_cacheable_when_cache_configured() {
        let messages = vec![
            Message::new(MessageRole::System, "You are a helpful agent.".to_string()),
            Message::new(MessageRole::User, "Hi".to_string()),
        ];

        // WITH caching: system must serialize as an ARRAY carrying cache_control.
        let cached = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
            Some("1hour".to_string()),
            None,
            None,
        )
        .unwrap();
        let body = cached
            .create_request_body(&messages, None, false, 1000, false)
            .unwrap();
        let json = serde_json::to_value(&body).unwrap();
        let system = json.get("system").expect("system field must be present");
        assert!(
            system.is_array(),
            "With cache_config set, `system` must serialize as an array of content \
             blocks — a JSON string has nowhere to hang cache_control, so a string \
             here means the prefix is silently uncacheable. Got: {}",
            system
        );
        let block = &system[0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "You are a helpful agent.");
        assert_eq!(
            block["cache_control"]["type"], "ephemeral",
            "system block must carry cache_control"
        );
        assert_eq!(
            block["cache_control"]["ttl"], "1h",
            "cache_config = \"1hour\" must map to the API's \"1h\" TTL"
        );

        // WITHOUT caching: the legacy string shape, byte for byte. A provider
        // that never caches must not start emitting a new wire format.
        let plain = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let json = serde_json::to_value(
            &plain
                .create_request_body(&messages, None, false, 1000, false)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json["system"], "You are a helpful agent.",
            "With caching off, `system` must remain a plain string"
        );
        assert!(
            !serde_json::to_string(&json).unwrap().contains("cache_control"),
            "No cache_control may appear anywhere when caching is unconfigured"
        );

        // A GARBAGE cache_config must degrade to no-cache, never panic. A typo
        // in a cost setting must not be able to take the agent down.
        let typo = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
            Some("1 hour".to_string()),
            None,
            None,
        )
        .unwrap();
        let json = serde_json::to_value(
            &typo
                .create_request_body(&messages, None, false, 1000, false)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json["system"], "You are a helpful agent.",
            "An unrecognised cache_config must fall back to the uncached string \
             form, not error and not emit a malformed block"
        );
    }

    /// An EMPTY system prompt must not produce a cached block.
    ///
    /// Boundary case with teeth: only four cache breakpoints exist per request,
    /// and a zero-length segment is below the API's minimum cacheable length —
    /// so caching an empty prompt spends a scarce breakpoint to cache nothing,
    /// and may be rejected outright.
    #[test]
    fn test_empty_system_prompt_is_not_cached() {
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            None,
            None,
            None,
            Some("1hour".to_string()),
            None,
            None,
        )
        .unwrap();

        let messages = vec![
            Message::new(MessageRole::System, "   \n  ".to_string()),
            Message::new(MessageRole::User, "Hi".to_string()),
        ];
        let json = serde_json::to_value(
            &provider
                .create_request_body(&messages, None, false, 1000, false)
                .unwrap(),
        )
        .unwrap();
        assert!(
            json["system"].is_string(),
            "A whitespace-only system prompt must stay a plain string, not become \
             an empty cached block. Got: {}",
            json["system"]
        );

        // No system message at all: the field must be omitted entirely.
        let no_system = vec![Message::new(MessageRole::User, "Hi".to_string())];
        let json = serde_json::to_value(
            &provider
                .create_request_body(&no_system, None, false, 1000, false)
                .unwrap(),
        )
        .unwrap();
        assert!(
            json.get("system").is_none(),
            "With no system message the `system` field must be absent"
        );
    }

    #[test]
    fn test_tool_conversion() {
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
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
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
                .unwrap();

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
            Some(1000),
            Some(0.5),
            None,
            None,
            None, // No thinking budget
        )
        .unwrap();

        let messages = vec![Message::new(MessageRole::User, "Test message".to_string())];
        let request_without = provider_without
            .create_request_body(&messages, None, false, 1000, false)
            .unwrap();
        let json_without = serde_json::to_string(&request_without).unwrap();
        assert!(
            !json_without.contains("thinking"),
            "JSON should not contain 'thinking' field when not configured"
        );

        // Test WITH thinking parameter - max_tokens must be > budget_tokens + 1024
        // Using budget=10000 requires max_tokens > 11024
        let provider_with = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
            Some(20000), // Sufficient for thinking budget
            Some(0.5),
            None,
            None,
            Some(10000), // With thinking budget
        )
        .unwrap();

        let request_with = provider_with
            .create_request_body(&messages, None, false, 20000, false)
            .unwrap();
        let json_with = serde_json::to_string(&request_with).unwrap();
        assert!(
            json_with.contains("thinking"),
            "JSON should contain 'thinking' field when configured"
        );
        assert!(
            json_with.contains("\"type\":\"enabled\""),
            "JSON should contain type: enabled"
        );
        assert!(
            json_with.contains("\"budget_tokens\":10000"),
            "JSON should contain budget_tokens: 10000"
        );

        // Test WITH thinking parameter but INSUFFICIENT max_tokens - thinking should be disabled
        let request_insufficient = provider_with
            .create_request_body(&messages, None, false, 5000, false) // Less than budget + 1024
            .unwrap();
        let json_insufficient = serde_json::to_string(&request_insufficient).unwrap();
        assert!(
            !json_insufficient.contains("thinking"),
            "JSON should NOT contain 'thinking' field when max_tokens is insufficient"
        );
    }

    #[test]
    fn test_disable_thinking_flag() {
        // Test that disable_thinking=true prevents thinking even with sufficient max_tokens
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            Some("claude-sonnet-4-5".to_string()),
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
            .create_request_body(&messages, None, false, 20000, false)
            .unwrap();
        let json_with = serde_json::to_string(&request_with_thinking).unwrap();
        assert!(
            json_with.contains("thinking"),
            "JSON should contain 'thinking' field when not disabled"
        );

        // With disable_thinking=true, thinking should be disabled even with sufficient max_tokens
        let request_without_thinking = provider
            .create_request_body(&messages, None, false, 20000, true)
            .unwrap();
        let json_without = serde_json::to_string(&request_without_thinking).unwrap();
        assert!(
            !json_without.contains("thinking"),
            "JSON should NOT contain 'thinking' field when explicitly disabled"
        );
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
        let text_content: Vec<_> = response
            .content
            .iter()
            .filter_map(|c| match c {
                AnthropicContent::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(text_content.len(), 1);
        assert_eq!(text_content[0], "Here is my response.");
    }

    // ====================================================================
    // Orphaned tool_use stripping tests
    // ====================================================================

    #[test]
    fn test_strip_orphaned_tool_use_removes_orphaned_blocks() {
        // Simulate: assistant with tool_use, followed by regular user message (no tool_result)
        let mut messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Read the file".to_string(),
                    cache_control: None,
                }],
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![
                    AnthropicContent::Text {
                        text: "Let me read that.".to_string(),
                        cache_control: None,
                    },
                    AnthropicContent::ToolUse {
                        id: "toolu_orphaned".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"file_path": "test.rs"}),
                    },
                ],
            },
            // Next message is a regular user message, NOT a tool_result
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Do something else".to_string(),
                    cache_control: None,
                }],
            },
        ];

        AnthropicProvider::strip_orphaned_tool_use(&mut messages);

        // The tool_use should be stripped from the assistant message
        let assistant = &messages[1];
        assert!(
            !assistant.content.iter().any(|c| matches!(c, AnthropicContent::ToolUse { .. })),
            "Orphaned tool_use should be stripped"
        );
        // Text content should remain
        assert!(
            assistant.content.iter().any(|c| matches!(c, AnthropicContent::Text { .. })),
            "Text content should be preserved"
        );
    }

    #[test]
    fn test_strip_orphaned_tool_use_preserves_valid_sequence() {
        // Valid: assistant with tool_use, followed by user with matching tool_result
        let mut messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Read the file".to_string(),
                    cache_control: None,
                }],
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![
                    AnthropicContent::Text {
                        text: "Reading...".to_string(),
                        cache_control: None,
                    },
                    AnthropicContent::ToolUse {
                        id: "toolu_valid".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"file_path": "test.rs"}),
                    },
                ],
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::ToolResult {
                    tool_use_id: "toolu_valid".to_string(),
                    content: ToolResultContent::Text("file contents".to_string()),
                    cache_control: None,
                }],
            },
        ];

        AnthropicProvider::strip_orphaned_tool_use(&mut messages);

        // tool_use should NOT be stripped
        let assistant = &messages[1];
        assert!(
            assistant.content.iter().any(|c| matches!(c, AnthropicContent::ToolUse { .. })),
            "Valid tool_use should be preserved"
        );
    }

    #[test]
    fn test_strip_orphaned_tool_use_adds_placeholder_for_empty_message() {
        // Assistant message with ONLY a tool_use block (no text)
        let mut messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Do something".to_string(),
                    cache_control: None,
                }],
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![AnthropicContent::ToolUse {
                    id: "toolu_only".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Never mind".to_string(),
                    cache_control: None,
                }],
            },
        ];

        AnthropicProvider::strip_orphaned_tool_use(&mut messages);

        // Should have placeholder text instead of empty content
        let assistant = &messages[1];
        assert!(!assistant.content.is_empty(), "Should not have empty content");
        assert!(
            assistant.content.iter().any(|c| matches!(c, AnthropicContent::Text { .. })),
            "Should have placeholder text"
        );
        assert!(
            !assistant.content.iter().any(|c| matches!(c, AnthropicContent::ToolUse { .. })),
            "tool_use should be stripped"
        );
    }

    #[test]
    fn test_tool_result_with_images_nested_inside() {
        // When a tool result message has images (e.g., from read_image),
        // the images must be nested inside the tool_result content array,
        // NOT as top-level Image blocks alongside the ToolResult block.
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
                .unwrap();

        let mut msg = Message::new(
            MessageRole::User,
            "Tool result: 2 image(s) read.".to_string(),
        );
        msg.tool_result_id = Some("toolu_01JQBMs7hdNpy3VBiJkJwykC".to_string());
        msg.images = vec![
            crate::ImageContent::new("image/png", "base64data1".to_string()),
            crate::ImageContent::new("image/jpeg", "base64data2".to_string()),
        ];

        // Also need an assistant message with the tool_use before it
        let mut assistant_msg = Message::new(MessageRole::Assistant, String::new());
        assistant_msg.tool_calls.push(crate::MessageToolCall {
            id: "toolu_01JQBMs7hdNpy3VBiJkJwykC".to_string(),
            name: "read_image".to_string(),
            input: serde_json::json!({"file_paths": ["a.png", "b.jpg"]}),
        });

        let messages = vec![
            Message::new(MessageRole::User, "Read these images".to_string()),
            assistant_msg,
            msg,
        ];

        let (_, anthropic_messages) = provider.convert_messages(&messages).unwrap();

        // The user tool result message should have exactly ONE content block (the ToolResult)
        let tool_result_msg = &anthropic_messages[2];
        assert_eq!(tool_result_msg.role, "user");
        assert_eq!(
            tool_result_msg.content.len(),
            1,
            "Should have exactly 1 content block (ToolResult), not images + ToolResult"
        );

        // Verify it's a ToolResult with structured content
        match &tool_result_msg.content[0] {
            AnthropicContent::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                assert_eq!(tool_use_id, "toolu_01JQBMs7hdNpy3VBiJkJwykC");
                match content {
                    ToolResultContent::Blocks(blocks) => {
                        assert_eq!(blocks.len(), 3, "Should have 2 images + 1 text block");
                        // First two should be images
                        assert!(matches!(&blocks[0], ToolResultBlock::Image { .. }));
                        assert!(matches!(&blocks[1], ToolResultBlock::Image { .. }));
                        // Last should be text
                        match &blocks[2] {
                            ToolResultBlock::Text { text } => {
                                assert_eq!(text, "Tool result: 2 image(s) read.");
                            }
                            _ => panic!("Expected text block at position 2"),
                        }
                    }
                    ToolResultContent::Text(_) => {
                        panic!("Expected Blocks content for tool result with images");
                    }
                }
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        // Verify no top-level Image blocks in the message
        assert!(
            !tool_result_msg
                .content
                .iter()
                .any(|c| matches!(c, AnthropicContent::Image { .. })),
            "Images should NOT be top-level blocks in a tool_result message"
        );

        // Verify the JSON serialization looks correct
        let json = serde_json::to_value(&tool_result_msg).unwrap();
        let content_arr = json["content"].as_array().unwrap();
        assert_eq!(content_arr.len(), 1);
        let tr = &content_arr[0];
        assert_eq!(tr["type"], "tool_result");
        assert_eq!(tr["tool_use_id"], "toolu_01JQBMs7hdNpy3VBiJkJwykC");
        // content should be an array (structured), not a string
        assert!(
            tr["content"].is_array(),
            "tool_result content should be an array when images present"
        );
        let inner = tr["content"].as_array().unwrap();
        assert_eq!(inner.len(), 3);
        assert_eq!(inner[0]["type"], "image");
        assert_eq!(inner[1]["type"], "image");
        assert_eq!(inner[2]["type"], "text");
    }

    #[test]
    fn test_tool_result_without_images_uses_string_content() {
        // Regular tool results (no images) should still use simple string content
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
                .unwrap();

        let mut msg = Message::new(
            MessageRole::User,
            "Tool result: file contents here".to_string(),
        );
        msg.tool_result_id = Some("toolu_abc123".to_string());
        // No images

        let mut assistant_msg = Message::new(MessageRole::Assistant, String::new());
        assistant_msg.tool_calls.push(crate::MessageToolCall {
            id: "toolu_abc123".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"file_path": "test.rs"}),
        });

        let messages = vec![
            Message::new(MessageRole::User, "Read the file".to_string()),
            assistant_msg,
            msg,
        ];

        let (_, anthropic_messages) = provider.convert_messages(&messages).unwrap();

        let tool_result_msg = &anthropic_messages[2];
        assert_eq!(tool_result_msg.content.len(), 1);

        // Verify the JSON has string content (not array)
        let json = serde_json::to_value(&tool_result_msg).unwrap();
        let tr = &json["content"][0];
        assert_eq!(tr["type"], "tool_result");
        assert!(
            tr["content"].is_string(),
            "tool_result content should be a string when no images: got {:?}",
            tr["content"]
        );
        assert_eq!(tr["content"], "Tool result: file contents here");
    }

    #[test]
    fn test_regular_user_message_with_images_uses_top_level_blocks() {
        // Non-tool-result user messages should still have images as top-level blocks
        let provider =
            AnthropicProvider::new("test-key".to_string(), None, None, None, None, None, None)
                .unwrap();

        let mut msg = Message::new(MessageRole::User, "What's in this image?".to_string());
        // No tool_result_id — this is a regular user message
        msg.images = vec![crate::ImageContent::new(
            "image/png",
            "base64data".to_string(),
        )];

        let messages = vec![msg];
        let (_, anthropic_messages) = provider.convert_messages(&messages).unwrap();

        let user_msg = &anthropic_messages[0];
        assert_eq!(user_msg.content.len(), 2, "Should have Image + Text blocks");
        assert!(matches!(
            &user_msg.content[0],
            AnthropicContent::Image { .. }
        ));
        assert!(matches!(
            &user_msg.content[1],
            AnthropicContent::Text { .. }
        ));
    }

    #[test]
    fn test_strip_orphaned_tool_use_works_with_structured_tool_result() {
        // Ensure orphan detection still works when tool_result has structured content
        let mut messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::Text {
                    text: "Read images".to_string(),
                    cache_control: None,
                }],
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: vec![AnthropicContent::ToolUse {
                    id: "toolu_img".to_string(),
                    name: "read_image".to_string(),
                    input: serde_json::json!({"file_paths": ["a.png"]}),
                }],
            },
            AnthropicMessage {
                role: "user".to_string(),
                content: vec![AnthropicContent::ToolResult {
                    tool_use_id: "toolu_img".to_string(),
                    content: ToolResultContent::Blocks(vec![
                        ToolResultBlock::Image {
                            source: AnthropicImageSource {
                                source_type: "base64".to_string(),
                                media_type: "image/png".to_string(),
                                data: "data".to_string(),
                            },
                        },
                        ToolResultBlock::Text {
                            text: "1 image(s) read.".to_string(),
                        },
                    ]),
                    cache_control: None,
                }],
            },
        ];

        AnthropicProvider::strip_orphaned_tool_use(&mut messages);

        // tool_use should NOT be stripped — it has a matching tool_result
        let assistant = &messages[1];
        assert!(
            assistant
                .content
                .iter()
                .any(|c| matches!(c, AnthropicContent::ToolUse { .. })),
            "Valid tool_use with structured tool_result should be preserved"
        );
    }
}
