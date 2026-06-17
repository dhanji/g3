#![allow(dead_code)]
//! Mock LLM Provider for Testing
//!
//! This module provides a configurable mock provider that can simulate
//! various LLM behaviors for integration testing. It allows precise control
//! over streaming chunks, tool calls, and response patterns.
//!
//! # Example
//!
//! ```rust,ignore
//! use g3_providers::mock::{MockProvider, MockResponse};
//!
//! // Simple text-only response
//! let provider = MockProvider::new()
//!     .with_response(MockResponse::text("Hello, world!"));
//!
//! // Response with tool call
//! let provider = MockProvider::new()
//!     .with_response(MockResponse::tool_call("shell", json!({"command": "ls"})));
//!
//! // Multi-chunk streaming response
//! let provider = MockProvider::new()
//!     .with_response(MockResponse::streaming(vec![
//!         "Hello, ",
//!         "world!",
//!     ]));
//! ```

use crate::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider,
    ToolCall, Usage,
};
use anyhow::Result;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Global counter for generating unique tool call IDs
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A mock response that can be configured for testing
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// Chunks to stream (content, finished, tool_calls, stop_reason)
    pub chunks: Vec<MockChunk>,
    /// Usage stats to report
    pub usage: Usage,
}

/// A single chunk in a mock streaming response
#[derive(Debug, Clone)]
pub struct MockChunk {
    pub content: String,
    pub finished: bool,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub stop_reason: Option<String>,
    pub tool_call_streaming: Option<String>,
}

impl MockChunk {
    /// Create a content chunk (not finished)
    pub fn content(text: &str) -> Self {
        Self {
            content: text.to_string(),
            finished: false,
            tool_calls: None,
            stop_reason: None,
            tool_call_streaming: None,
        }
    }

    /// Create a final chunk with stop reason
    pub fn finished(stop_reason: &str) -> Self {
        Self {
            content: String::new(),
            finished: true,
            tool_calls: None,
            stop_reason: Some(stop_reason.to_string()),
            tool_call_streaming: None,
        }
    }

    /// Create a chunk with a tool call
    pub fn tool_call(tool: &str, args: serde_json::Value) -> Self {
        Self {
            content: String::new(),
            finished: false,
            tool_calls: Some(vec![ToolCall {
                id: format!("tool_{}", TOOL_CALL_COUNTER.fetch_add(1, Ordering::SeqCst)),
                tool: tool.to_string(),
                args,
            }]),
            stop_reason: None,
            tool_call_streaming: None,
        }
    }

    /// Create a chunk indicating tool call is streaming (for UI hint)
    pub fn tool_streaming(tool_name: &str) -> Self {
        Self {
            content: String::new(),
            finished: false,
            tool_calls: None,
            stop_reason: None,
            tool_call_streaming: Some(tool_name.to_string()),
        }
    }
}

impl MockResponse {
    /// Create a simple text-only response (single chunk + finish)
    pub fn text(content: &str) -> Self {
        Self {
            chunks: vec![
                MockChunk::content(content),
                MockChunk::finished("end_turn"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: content.len() as u32 / 4,
                total_tokens: 100 + content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a streaming text response with multiple chunks
    pub fn streaming(chunks: Vec<&str>) -> Self {
        let total_content: String = chunks.iter().copied().collect();
        let mut mock_chunks: Vec<MockChunk> = chunks
            .into_iter()
            .map(MockChunk::content)
            .collect();
        mock_chunks.push(MockChunk::finished("end_turn"));

        Self {
            chunks: mock_chunks,
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: total_content.len() as u32 / 4,
                total_tokens: 100 + total_content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with a native tool call
    pub fn native_tool_call(tool: &str, args: serde_json::Value) -> Self {
        Self {
            chunks: vec![
                MockChunk::tool_streaming(tool),
                MockChunk::tool_call(tool, args),
                MockChunk::finished("tool_use"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with text followed by a native tool call
    pub fn text_then_native_tool(text: &str, tool: &str, args: serde_json::Value) -> Self {
        Self {
            chunks: vec![
                MockChunk::content(text),
                MockChunk::tool_streaming(tool),
                MockChunk::tool_call(tool, args),
                MockChunk::finished("tool_use"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 50 + text.len() as u32 / 4,
                total_tokens: 150 + text.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with duplicate native tool calls (same tool called twice)
    /// Used to test duplicate detection
    pub fn duplicate_native_tool_calls(tool: &str, args: serde_json::Value) -> Self {
        Self {
            chunks: vec![
                MockChunk::tool_streaming(tool),
                MockChunk::tool_call(tool, args.clone()),
                // Second identical tool call
                MockChunk::tool_streaming(tool),
                MockChunk::tool_call(tool, args),
                MockChunk::finished("tool_use"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 100,
                total_tokens: 200,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with text followed by a JSON tool call (non-native)
    pub fn text_with_json_tool(text: &str, tool: &str, args: serde_json::Value) -> Self {
        // Manually construct JSON to ensure "tool" comes before "args"
        // (serde_json::json! alphabetizes keys, which breaks pattern detection)
        let args_str = serde_json::to_string(&args).unwrap();
        let tool_str = format!(r#"{{"tool": "{}", "args": {}}}"#, tool, args_str);
        let full_content = format!("{}\n\n{}", text, tool_str);

        Self {
            chunks: vec![
                MockChunk::content(text),
                MockChunk::content("\n\n"),
                MockChunk::content(&tool_str),
                MockChunk::finished("end_turn"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: full_content.len() as u32 / 4,
                total_tokens: 100 + full_content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with duplicate JSON tool calls in text content (non-native).
    /// Mimics the LLM stuttering pattern where it emits the same tool call twice.
    pub fn text_with_duplicate_json_tools(tool: &str, args: serde_json::Value) -> Self {
        let args_str = serde_json::to_string(&args).unwrap();
        let tool_str = format!(r#"{{"tool": "{}", "args": {}}}"#, tool, args_str);
        let full_content = format!("{}\n\n{}", &tool_str, &tool_str);

        Self {
            chunks: vec![
                MockChunk::content(&tool_str),
                MockChunk::content("\n\n"),
                MockChunk::content(&tool_str),
                MockChunk::finished("end_turn"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: full_content.len() as u32 / 4,
                total_tokens: 100 + full_content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a response with text followed by duplicate JSON tool calls (non-native).
    /// Mimics the pattern: "Let me run that.\n\n{tool...}\n\n{tool...}"
    pub fn text_with_duplicate_json_tools_prefixed(
        text: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Self {
        let args_str = serde_json::to_string(&args).unwrap();
        let tool_str = format!(r#"{{"tool": "{}", "args": {}}}"#, tool, args_str);
        let full_content = format!("{}\n\n{}\n\n{}", text, &tool_str, &tool_str);

        Self {
            chunks: vec![
                MockChunk::content(text),
                MockChunk::content("\n\n"),
                MockChunk::content(&tool_str),
                MockChunk::content("\n\n"),
                MockChunk::content(&tool_str),
                MockChunk::finished("end_turn"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: full_content.len() as u32 / 4,
                total_tokens: 100 + full_content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }
    pub fn truncated(content: &str) -> Self {
        Self {
            chunks: vec![
                MockChunk::content(content),
                MockChunk::finished("max_tokens"),
            ],
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: content.len() as u32 / 4,
                total_tokens: 100 + content.len() as u32 / 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        }
    }

    /// Create a custom response with explicit chunks
    pub fn custom(chunks: Vec<MockChunk>, usage: Usage) -> Self {
        Self { chunks, usage }
    }

    /// Builder: set custom usage
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = usage;
        self
    }
}

/// A mock LLM provider for testing
///
/// The provider maintains a queue of responses that are returned in order.
/// It also tracks all requests made for verification in tests.
pub struct MockProvider {
    name: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    native_tool_calling: bool,
    supports_cache_control: bool,
    /// Queue of responses to return (FIFO)
    responses: Arc<Mutex<Vec<MockResponse>>>,
    /// All requests received (for verification)
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    /// Default response when queue is empty
    default_response: Option<MockResponse>,
}

impl MockProvider {
    /// Create a new mock provider with default settings
    pub fn new() -> Self {
        Self {
            name: "mock".to_string(),
            model: "mock-model".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            native_tool_calling: false,
            supports_cache_control: false,
            responses: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            default_response: None,
        }
    }

    /// Set the provider name
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set the model name
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    /// Set max tokens
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    /// Enable native tool calling
    pub fn with_native_tool_calling(mut self, enabled: bool) -> Self {
        self.native_tool_calling = enabled;
        self
    }

    /// Enable cache_control support (as the Anthropic provider reports).
    pub fn with_cache_control_support(mut self, enabled: bool) -> Self {
        self.supports_cache_control = enabled;
        self
    }

    /// Add a response to the queue
    pub fn with_response(self, response: MockResponse) -> Self {
        self.responses.lock().unwrap().push(response);
        self
    }

    /// Add multiple responses to the queue
    pub fn with_responses(self, responses: Vec<MockResponse>) -> Self {
        self.responses.lock().unwrap().extend(responses);
        self
    }

    /// Set a default response when queue is empty
    pub fn with_default_response(mut self, response: MockResponse) -> Self {
        self.default_response = Some(response);
        self
    }

    /// Get all requests that were made to this provider
    pub fn get_requests(&self) -> Vec<CompletionRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Get the number of requests made
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// Clear recorded requests
    pub fn clear_requests(&self) {
        self.requests.lock().unwrap().clear();
    }

    /// Get the next response from the queue (or default)
    fn next_response(&self) -> MockResponse {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            self.default_response
                .clone()
                .unwrap_or_else(|| MockResponse::text("Mock response (no responses configured)"))
        } else {
            responses.remove(0)
        }
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LLMProvider for MockProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        // Record the request
        self.requests.lock().unwrap().push(request);

        let response = self.next_response();

        // Combine all chunk content for non-streaming response
        let content: String = response
            .chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect();

        Ok(CompletionResponse {
            content,
            usage: response.usage,
            model: self.model.clone(),
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        // Record the request
        self.requests.lock().unwrap().push(request);

        let response = self.next_response();
        let usage = response.usage.clone();

        // Create a channel for streaming
        let (tx, rx) = mpsc::channel(32);
        let num_chunks = response.chunks.len();

        // Spawn a task to send chunks
        tokio::spawn(async move {
            for (i, chunk) in response.chunks.into_iter().enumerate() {
                let is_last = chunk.finished;
                let completion_chunk = CompletionChunk {
                    content: chunk.content,
                    finished: chunk.finished,
                    tool_calls: chunk.tool_calls,
                    usage: if is_last { Some(usage.clone()) } else { None },
                    stop_reason: chunk.stop_reason,
                    tool_call_streaming: chunk.tool_call_streaming,
                };

                if tx.send(Ok(completion_chunk)).await.is_err() {
                    // Receiver dropped, stop sending
                    break;
                }

                // Small delay between chunks to simulate streaming
                if i < num_chunks - 1 {
                    tokio::time::sleep(tokio::time::Duration::from_micros(100)).await;
                }
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
        self.native_tool_calling
    }

    fn supports_cache_control(&self) -> bool {
        self.supports_cache_control
    }

    fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    fn temperature(&self) -> f32 {
        self.temperature
    }
}

// ============================================================================
// Preset Scenarios for Common Test Cases
// ============================================================================

/// Preset scenarios for common testing patterns
pub mod scenarios {
    use super::*;

    /// Create a provider that returns a simple text response
    /// This simulates the bug scenario where text-only responses weren't saved
    pub fn text_only_response(text: &str) -> MockProvider {
        MockProvider::new().with_response(MockResponse::text(text))
    }

    /// Create a provider that returns text followed by a tool call
    pub fn text_then_tool(text: &str, tool: &str, args: serde_json::Value) -> MockProvider {
        MockProvider::new().with_response(MockResponse::text_with_json_tool(text, tool, args))
    }

    /// Create a provider for multi-turn conversation
    /// Each call returns the next response in sequence
    pub fn multi_turn(responses: Vec<&str>) -> MockProvider {
        let mock_responses: Vec<MockResponse> =
            responses.into_iter().map(MockResponse::text).collect();
        MockProvider::new().with_responses(mock_responses)
    }

    /// Create a provider that simulates tool execution flow:
    /// 1. First call: returns tool call
    /// 2. Second call: returns text response after tool result
    pub fn tool_then_response(
        tool: &str,
        args: serde_json::Value,
        final_response: &str,
    ) -> MockProvider {
        MockProvider::new()
            .with_native_tool_calling(true)
            .with_responses(vec![
                MockResponse::native_tool_call(tool, args),
                MockResponse::text(final_response),
            ])
    }

    /// Create a provider that returns a truncated response (max_tokens hit)
    pub fn truncated_response(partial_content: &str) -> MockProvider {
        MockProvider::new().with_response(MockResponse::truncated(partial_content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn test_mock_provider_text_response() {
        let provider = MockProvider::new().with_response(MockResponse::text("Hello, world!"));

        let request = CompletionRequest {
            messages: vec![],
            max_tokens: None,
            temperature: None,
            stream: false,
            tools: None,
            disable_thinking: false,
        };

        let response = provider.complete(request).await.unwrap();
        assert_eq!(response.content, "Hello, world!");
        assert_eq!(provider.request_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_streaming() {
        let provider =
            MockProvider::new().with_response(MockResponse::streaming(vec!["Hello, ", "world!"]));

        let request = CompletionRequest {
            messages: vec![],
            max_tokens: None,
            temperature: None,
            stream: true,
            tools: None,
            disable_thinking: false,
        };

        let mut stream = provider.stream(request).await.unwrap();

        let mut content = String::new();
        let mut chunk_count = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            content.push_str(&chunk.content);
            chunk_count += 1;
        }

        assert_eq!(content, "Hello, world!");
        assert_eq!(chunk_count, 3); // 2 content chunks + 1 finish chunk
    }

    #[tokio::test]
    async fn test_mock_provider_multi_turn() {
        let provider = scenarios::multi_turn(vec!["First response", "Second response"]);

        let request = CompletionRequest {
            messages: vec![],
            max_tokens: None,
            temperature: None,
            stream: false,
            tools: None,
            disable_thinking: false,
        };

        let response1 = provider.complete(request.clone()).await.unwrap();
        assert_eq!(response1.content, "First response");

        let response2 = provider.complete(request).await.unwrap();
        assert_eq!(response2.content, "Second response");
    }

    #[tokio::test]
    async fn test_mock_provider_tool_call() {
        let provider = MockProvider::new()
            .with_native_tool_calling(true)
            .with_response(MockResponse::native_tool_call(
                "shell",
                serde_json::json!({"command": "ls"}),
            ));

        let request = CompletionRequest {
            messages: vec![],
            max_tokens: None,
            temperature: None,
            stream: true,
            tools: None,
            disable_thinking: false,
        };

        let mut stream = provider.stream(request).await.unwrap();

        let mut found_tool_call = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if let Some(tool_calls) = chunk.tool_calls {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].tool, "shell");
                found_tool_call = true;
            }
        }

        assert!(found_tool_call, "Should have received a tool call");
    }

    #[tokio::test]
    async fn test_mock_provider_request_tracking() {
        let provider = MockProvider::new().with_default_response(MockResponse::text("OK"));

        let request1 = CompletionRequest {
            messages: vec![crate::Message::new(crate::MessageRole::User, "Hello".to_string())],
            max_tokens: Some(100),
            temperature: None,
            stream: false,
            tools: None,
            disable_thinking: false,
        };

        let request2 = CompletionRequest {
            messages: vec![crate::Message::new(
                crate::MessageRole::User,
                "World".to_string(),
            )],
            max_tokens: Some(200),
            temperature: None,
            stream: false,
            tools: None,
            disable_thinking: false,
        };

        provider.complete(request1).await.unwrap();
        provider.complete(request2).await.unwrap();

        let requests = provider.get_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].max_tokens, Some(100));
        assert_eq!(requests[1].max_tokens, Some(200));
    }
}
