//! Google Gemini provider implementation for the g3-providers crate.
//!
//! This module provides an implementation of the `LLMProvider` trait for Google's Gemini models,
//! supporting both completion and streaming modes through the Gemini API.
//!
//! # Features
//!
//! - Support for Gemini models (gemini-2.0-flash, gemini-1.5-pro, etc.)
//! - Both completion and streaming response modes
//! - Proper message format conversion between g3 and Gemini formats
//! - Native tool calling support
//!
//! # Usage
//!
//! ```rust,no_run
//! use g3_providers::{GeminiProvider, LLMProvider, CompletionRequest, Message, MessageRole};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let provider = GeminiProvider::new(
//!         "your-api-key".to_string(),
//!         Some("gemini-2.0-flash".to_string()),
//!         Some(8192),
//!         Some(0.7),
//!     )?;
//!
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
//!     let response = provider.complete(request).await?;
//!     println!("Response: {}", response.content);
//!
//!     Ok(())
//! }
//! ```

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error};

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider, Message,
    MessageRole, Tool, ToolCall, Usage, streaming::make_text_chunk,
};

// ============================================================================
// Provider Struct
// ============================================================================

#[derive(Clone)]
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    name: String,
}

impl GeminiProvider {
    pub fn new(
        api_key: String,
        model: Option<String>,
        max_tokens: Option<u32>,
        temperature: Option<f32>,
    ) -> Result<Self> {
        Self::new_with_name("gemini".to_string(), api_key, model, max_tokens, temperature)
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
            model: model.unwrap_or_else(|| "gemini-2.0-flash".to_string()),
            max_tokens: max_tokens.unwrap_or(16384),
            temperature: temperature.unwrap_or(0.1),
            name,
        })
    }

    fn get_api_url(&self, stream: bool) -> String {
        let method = if stream { "streamGenerateContent" } else { "generateContent" };
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:{}?key={}",
            self.model, method, self.api_key
        )
    }
}

// ============================================================================
// Gemini API Request/Response Types
// ============================================================================

/// Gemini API request body
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    generation_config: GeminiGenerationConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
}

/// Gemini API response
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
    total_token_count: Option<u32>,
}

// ============================================================================
// Message Conversion
// ============================================================================

/// Convert g3 messages to Gemini format
/// 
/// Key differences:
/// - Gemini uses "model" instead of "assistant"
/// - System messages go in system_instruction, not contents
/// - Gemini uses "parts" array with text objects
fn convert_messages(messages: &[Message]) -> (Vec<GeminiContent>, Option<GeminiContent>) {
    let mut contents = Vec::new();
    let mut system_instruction = None;

    for msg in messages {
        match msg.role {
            MessageRole::System => {
                // System messages go to system_instruction
                system_instruction = Some(GeminiContent {
                    role: None, // system_instruction doesn't need a role
                    parts: vec![GeminiPart::Text { text: msg.content.clone() }],
                });
            }
            MessageRole::User => {
                contents.push(GeminiContent {
                    role: Some("user".to_string()),
                    parts: vec![GeminiPart::Text { text: msg.content.clone() }],
                });
            }
            MessageRole::Assistant => {
                // Gemini uses "model" instead of "assistant"
                contents.push(GeminiContent {
                    role: Some("model".to_string()),
                    parts: vec![GeminiPart::Text { text: msg.content.clone() }],
                });
            }
        }
    }

    (contents, system_instruction)
}

/// Convert g3 tools to Gemini format
fn convert_tools(tools: &[Tool]) -> Vec<GeminiTool> {
    let declarations: Vec<GeminiFunctionDeclaration> = tools
        .iter()
        .map(|tool| GeminiFunctionDeclaration {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: if tool.input_schema.is_null() {
                None
            } else {
                Some(tool.input_schema.clone())
            },
        })
        .collect();

    vec![GeminiTool {
        function_declarations: declarations,
    }]
}

/// Extract text content from Gemini response parts
fn extract_text_from_parts(parts: &[GeminiPart]) -> String {
    parts
        .iter()
        .filter_map(|part| {
            if let GeminiPart::Text { text } = part {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Extract tool calls from Gemini response parts
fn extract_tool_calls_from_parts(parts: &[GeminiPart]) -> Vec<ToolCall> {
    parts
        .iter()
        .filter_map(|part| {
            if let GeminiPart::FunctionCall { function_call } = part {
                Some(ToolCall {
                    id: format!("call_{}", nanoid::nanoid!(8)),
                    tool: function_call.name.clone(),
                    args: function_call.args.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Convert Gemini usage metadata to g3 Usage
fn convert_usage(metadata: Option<&GeminiUsageMetadata>) -> Usage {
    match metadata {
        Some(m) => Usage {
            prompt_tokens: m.prompt_token_count.unwrap_or(0),
            completion_tokens: m.candidates_token_count.unwrap_or(0),
            total_tokens: m.total_token_count.unwrap_or(0),
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
        None => Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
    }
}

/// Convert Gemini finish reason to g3 stop reason
fn convert_finish_reason(reason: Option<&str>) -> Option<String> {
    reason.map(|r| match r {
        "STOP" => "end_turn".to_string(),
        "MAX_TOKENS" => "max_tokens".to_string(),
        "SAFETY" => "content_filter".to_string(),
        "RECITATION" => "content_filter".to_string(),
        other => other.to_lowercase(),
    })
}

// ============================================================================
// Streaming Parser
// ============================================================================

/// Parse a streaming chunk from Gemini's SSE response
/// 
/// Gemini streams JSON objects, one per line (not SSE format with "data:" prefix)
fn parse_streaming_chunk(data: &str) -> Option<(String, Option<Vec<ToolCall>>, Option<String>, Option<GeminiUsageMetadata>)> {
    // Skip empty lines
    let data = data.trim();
    if data.is_empty() {
        return None;
    }

    // Try to parse as JSON
    let response: GeminiResponse = match serde_json::from_str(data) {
        Ok(r) => r,
        Err(e) => {
            debug!("Failed to parse Gemini streaming chunk: {} - data: {}", e, data);
            return None;
        }
    };

    // Extract content from candidates
    let candidates = response.candidates?;
    let candidate = candidates.first()?;
    let content = candidate.content.as_ref()?;
    
    let text = extract_text_from_parts(&content.parts);
    let tool_calls = extract_tool_calls_from_parts(&content.parts);
    let finish_reason = convert_finish_reason(candidate.finish_reason.as_deref());
    
    Some((
        text,
        if tool_calls.is_empty() { None } else { Some(tool_calls) },
        finish_reason,
        response.usage_metadata,
    ))
}

/// Process streaming response from Gemini
async fn process_stream(
    mut response: reqwest::Response,
    tx: mpsc::Sender<Result<CompletionChunk>>,
) {
    let mut buffer = String::new();
    let mut accumulated_text = String::new();
    let mut last_usage: Option<GeminiUsageMetadata> = None;
    let mut last_finish_reason: Option<String> = None;
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();

    while let Some(chunk_result) = response.chunk().await.transpose() {
        match chunk_result {
            Ok(bytes) => {
                let text = match String::from_utf8(bytes.to_vec()) {
                    Ok(t) => t,
                    Err(e) => {
                        error!("Invalid UTF-8 in Gemini stream: {}", e);
                        continue;
                    }
                };

                buffer.push_str(&text);

                // Gemini streams as JSON array elements or newline-delimited JSON
                // Try to parse complete JSON objects from the buffer
                while let Some(parsed) = try_parse_json_from_buffer(&mut buffer) {
                    if let Some((content, tool_calls, finish_reason, usage)) = parse_streaming_chunk(&parsed) {
                        // Track usage and finish reason
                        if usage.is_some() {
                            last_usage = usage;
                        }
                        if finish_reason.is_some() {
                            last_finish_reason = finish_reason;
                        }

                        // Handle tool calls
                        if let Some(calls) = tool_calls {
                            pending_tool_calls.extend(calls);
                        }

                        // Send text content
                        if !content.is_empty() {
                            accumulated_text.push_str(&content);
                            if tx.send(Ok(make_text_chunk(content))).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Error reading Gemini stream: {}", e);
                let _ = tx.send(Err(anyhow::anyhow!("Stream error: {}", e))).await;
                return;
            }
        }
    }

    // Send any pending tool calls
    if !pending_tool_calls.is_empty() {
        let chunk = CompletionChunk {
            content: String::new(),
            finished: false,
            tool_calls: Some(pending_tool_calls),
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };
        if tx.send(Ok(chunk)).await.is_err() {
            return;
        }
    }

    // Send final chunk with usage
    let final_chunk = CompletionChunk {
        content: String::new(),
        finished: true,
        tool_calls: None,
        usage: Some(convert_usage(last_usage.as_ref())),
        stop_reason: last_finish_reason,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    let _ = tx.send(Ok(final_chunk)).await;
}

/// Try to extract a complete JSON object from the buffer
/// 
/// Gemini streams responses as a JSON array: [{...}, {...}, ...]
/// We need to handle the array brackets and extract individual objects
fn try_parse_json_from_buffer(buffer: &mut String) -> Option<String> {
    let trimmed = buffer.trim_start();
    
    // Skip leading array bracket or comma
    let start_idx = if trimmed.starts_with('[') {
        buffer.find('[')? + 1
    } else if trimmed.starts_with(',') {
        buffer.find(',')? + 1
    } else {
        0
    };

    // Find the start of a JSON object
    let remaining = &buffer[start_idx..];
    let obj_start = remaining.find('{')?;
    let absolute_start = start_idx + obj_start;

    // Find matching closing brace
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;
    let mut end_idx = None;

    for (i, c) in buffer[absolute_start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match c {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    end_idx = Some(absolute_start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    if let Some(end) = end_idx {
        let json_str = buffer[absolute_start..end].to_string();
        *buffer = buffer[end..].to_string();
        Some(json_str)
    } else {
        None
    }
}

// ============================================================================
// LLMProvider Implementation
// ============================================================================

impl GeminiProvider {
    /// Build a GeminiRequest from a CompletionRequest.
    fn build_request(&self, request: &CompletionRequest) -> GeminiRequest {
        let (contents, system_instruction) = convert_messages(&request.messages);
        GeminiRequest {
            contents,
            system_instruction,
            tools: request.tools.as_ref().map(|t| convert_tools(t)),
            generation_config: GeminiGenerationConfig {
                max_output_tokens: request.max_tokens.or(Some(self.max_tokens)),
                temperature: request.temperature.or(Some(self.temperature)),
            },
        }
    }
}

#[async_trait]
impl LLMProvider for GeminiProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        let gemini_request = self.build_request(&request);

        let url = self.get_api_url(false);
        debug!("Gemini request URL: {}", url);
        debug!("Gemini request body: {}", serde_json::to_string_pretty(&gemini_request).unwrap_or_default());

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&gemini_request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Gemini API error ({}): {}", status, error_text);
            anyhow::bail!("Gemini API error ({}): {}", status, error_text);
        }

        let gemini_response: GeminiResponse = response.json().await?;
        debug!("Gemini response: {:?}", gemini_response);

        // Extract content from response
        let content = gemini_response
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .map(|c| extract_text_from_parts(&c.parts))
            .unwrap_or_default();

        let usage = convert_usage(gemini_response.usage_metadata.as_ref());

        Ok(CompletionResponse {
            content,
            usage,
            model: self.model.clone(),
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let gemini_request = self.build_request(&request);

        // For streaming, add alt=sse parameter
        let url = format!("{}&alt=sse", self.get_api_url(true));
        debug!("Gemini streaming request URL: {}", url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&gemini_request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!("Gemini API error ({}): {}", status, error_text);
            anyhow::bail!("Gemini API error ({}): {}", status, error_text);
        }

        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(process_stream(response, tx));

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

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn temperature(&self) -> f32 {
        self.temperature
    }

    fn context_window_size(&self) -> Option<u32> {
        // Context window sizes by model
        // https://ai.google.dev/gemini-api/docs/models
        let size = if self.model.contains("gemini-3") {
            1_000_000  // Gemini 3 models (assumed 1M, update when confirmed)
        } else if self.model.contains("1.5-pro") || self.model.contains("1.5-flash") {
            2_000_000  // Gemini 1.5 models have 2M context
        } else if self.model.contains("2.5-pro") || self.model.contains("2.5-flash") {
            1_000_000  // Gemini 2.5 models have 1M context
        } else if self.model.contains("2.0") {
            1_000_000  // Gemini 2.0 models have 1M context
        } else {
            128_000    // Conservative default for unknown models
        };
        Some(size)
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_convert_messages_basic() {
        let messages = vec![
            Message::new(MessageRole::User, "Hello".to_string()),
            Message::new(MessageRole::Assistant, "Hi there!".to_string()),
        ];

        let (contents, system) = convert_messages(&messages);

        assert!(system.is_none());
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role, Some("user".to_string()));
        assert_eq!(contents[1].role, Some("model".to_string())); // assistant -> model
    }

    #[test]
    fn test_convert_messages_with_system() {
        let messages = vec![
            Message::new(MessageRole::System, "You are helpful.".to_string()),
            Message::new(MessageRole::User, "Hello".to_string()),
        ];

        let (contents, system) = convert_messages(&messages);

        assert!(system.is_some());
        let sys = system.unwrap();
        assert!(sys.role.is_none()); // system_instruction has no role
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, Some("user".to_string()));
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![Tool {
            name: "get_weather".to_string(),
            description: "Get the weather".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                }
            }),
        }];

        let gemini_tools = convert_tools(&tools);

        assert_eq!(gemini_tools.len(), 1);
        assert_eq!(gemini_tools[0].function_declarations.len(), 1);
        assert_eq!(gemini_tools[0].function_declarations[0].name, "get_weather");
    }

    #[test]
    fn test_parse_streaming_chunk() {
        let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#;

        let result = parse_streaming_chunk(chunk);
        assert!(result.is_some());

        let (text, tool_calls, finish_reason, usage) = result.unwrap();
        assert_eq!(text, "Hello");
        assert!(tool_calls.is_none());
        assert_eq!(finish_reason, Some("end_turn".to_string()));
        assert!(usage.is_some());
        assert_eq!(usage.unwrap().total_token_count, Some(15));
    }

    #[test]
    fn test_parse_streaming_chunk_with_tool_call() {
        let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"location":"NYC"}}}],"role":"model"}}]}"#;

        let result = parse_streaming_chunk(chunk);
        assert!(result.is_some());

        let (text, tool_calls, _, _) = result.unwrap();
        assert_eq!(text, "");
        assert!(tool_calls.is_some());
        let calls = tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "get_weather");
    }

    #[test]
    fn test_try_parse_json_from_buffer() {
        let mut buffer = r#"[{"test": 1}, {"test": 2}]"#.to_string();
        
        let first = try_parse_json_from_buffer(&mut buffer);
        assert!(first.is_some());
        assert_eq!(first.unwrap(), r#"{"test": 1}"#);

        let second = try_parse_json_from_buffer(&mut buffer);
        assert!(second.is_some());
        assert_eq!(second.unwrap(), r#"{"test": 2}"#);
    }

    #[test]
    fn test_convert_finish_reason() {
        assert_eq!(convert_finish_reason(Some("STOP")), Some("end_turn".to_string()));
        assert_eq!(convert_finish_reason(Some("MAX_TOKENS")), Some("max_tokens".to_string()));
        assert_eq!(convert_finish_reason(Some("SAFETY")), Some("content_filter".to_string()));
        assert_eq!(convert_finish_reason(None), None);
    }

    #[test]
    fn test_extract_text_from_parts() {
        let parts = vec![
            GeminiPart::Text { text: "Hello ".to_string() },
            GeminiPart::Text { text: "world!".to_string() },
        ];

        let text = extract_text_from_parts(&parts);
        assert_eq!(text, "Hello world!");
    }
}
