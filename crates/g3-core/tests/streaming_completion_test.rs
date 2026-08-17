//! Streaming Completion Integration Test
//!
//! This test verifies that the Agent correctly processes a streaming response
//! containing multiple message types (TEXT, TOOL_CALL, TOOL_CALL, TEXT, TEXT)
//! and that control is not returned to the caller until all messages have been
//! processed and the stream signals completion.
//!
//! This protects against regressions where control might be returned mid-stream
//! after a single tool call, leaving subsequent messages unprocessed.

use anyhow::Result;
use async_trait::async_trait;
use g3_core::ui_writer::UiWriter;
use g3_core::Agent;
use g3_providers::{
    CompletionChunk, CompletionRequest, CompletionResponse, CompletionStream, LLMProvider,
    ProviderRegistry, ToolCall, Usage,
};
use serial_test::serial;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

// =============================================================================
// Mock Provider
// =============================================================================

/// A mock LLM provider that streams a predefined sequence of chunks.
/// On the FIRST call to stream(), it sends: TEXT -> TOOL_CALL -> TOOL_CALL -> TEXT -> TEXT -> FINISHED
/// On subsequent calls, it just sends a simple text response and finishes.
struct MockStreamingProvider {
    /// Counter to track how many times stream() has been called
    stream_call_count: Arc<AtomicUsize>,
    /// Flag set when the first stream has sent all 6 chunks including the finished signal
    first_stream_all_chunks_sent: Arc<AtomicBool>,
}

impl MockStreamingProvider {
    fn new() -> Self {
        Self {
            stream_call_count: Arc::new(AtomicUsize::new(0)),
            first_stream_all_chunks_sent: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(dead_code)]
    fn first_stream_completed(&self) -> bool {
        self.first_stream_all_chunks_sent.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn stream_call_count(&self) -> usize {
        self.stream_call_count.load(Ordering::SeqCst)
    }
}

fn default_usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
    }
}

#[async_trait]
impl LLMProvider for MockStreamingProvider {
    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            content: String::new(),
            usage: default_usage(),
            model: "mock".to_string(),
        })
    }

    async fn stream(&self, _request: CompletionRequest) -> Result<CompletionStream> {
        let call_num = self.stream_call_count.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(32);
        let first_stream_completed = self.first_stream_all_chunks_sent.clone();

        if call_num == 0 {
            // First call: send the full sequence with tool calls
            tokio::spawn(async move {
                // Chunk 1: Initial text
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: "I'll help you with that task. Let me ".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

                // Chunk 2: First tool call
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: String::new(),
                        finished: false,
                        tool_calls: Some(vec![ToolCall {
                            id: "call_1".to_string(),
                            tool: "shell".to_string(),
                            args: serde_json::json!({"command": "echo 'first tool call'"}),
                        }]),
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

                // Chunk 3: Second tool call
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: String::new(),
                        finished: false,
                        tool_calls: Some(vec![ToolCall {
                            id: "call_2".to_string(),
                            tool: "shell".to_string(),
                            args: serde_json::json!({"command": "echo 'second tool call'"}),
                        }]),
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

                // Chunk 4: More text
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: "Both commands executed. ".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

                // Chunk 5: Final text
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: "Done!".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

                // Chunk 6: Finished signal
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: String::new(),
                        finished: true,
                        tool_calls: None,
                        usage: Some(Usage {
                            prompt_tokens: 100,
                            completion_tokens: 50,
                            total_tokens: 150,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
                        }),
                        stop_reason: Some("end_turn".to_string()),
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                // Mark that we sent all chunks
                first_stream_completed.store(true, Ordering::SeqCst);
            });
        } else {
            // Subsequent calls: just send a simple completion
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: "Task complete.".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: String::new(),
                        finished: true,
                        tool_calls: None,
                        usage: Some(Usage {
                            prompt_tokens: 50,
                            completion_tokens: 10,
                            total_tokens: 60,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
                        }),
                        stop_reason: Some("end_turn".to_string()),
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;
            });
        }

        Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-streaming-model"
    }

    fn has_native_tool_calling(&self) -> bool {
        true
    }

    fn supports_cache_control(&self) -> bool {
        false
    }

    fn max_tokens(&self) -> u32 {
        4096
    }

    fn temperature(&self) -> f32 {
        0.0
    }
}

// =============================================================================
// Test UI Writer that tracks events
// =============================================================================

/// A UI writer that tracks tool call events for verification
#[derive(Clone)]
struct TrackingUiWriter {
    tool_calls_seen: Arc<AtomicUsize>,
    responses: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TrackingUiWriter {
    fn new(tool_calls_seen: Arc<AtomicUsize>) -> Self {
        Self {
            tool_calls_seen,
            responses: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn tool_call_count(&self) -> usize {
        self.tool_calls_seen.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn responses(&self) -> Vec<String> {
        self.responses.lock().unwrap().clone()
    }
}

impl UiWriter for TrackingUiWriter {
    fn print(&self, _message: &str) {}
    fn println(&self, _message: &str) {}
    fn print_inline(&self, _message: &str) {}
    fn print_system_prompt(&self, _prompt: &str) {}
    fn print_context_status(&self, _message: &str) {}
    fn print_g3_progress(&self, _message: &str) {}
    fn print_g3_status(&self, _message: &str, _status: &str) {}
    fn print_thin_result(&self, _result: &g3_core::ThinResult) {}

    fn print_tool_header(&self, _tool_name: &str, _tool_args: Option<&serde_json::Value>) {
        // Count each tool call
        self.tool_calls_seen.fetch_add(1, Ordering::SeqCst);
    }

    fn print_tool_arg(&self, _key: &str, _value: &str) {}
    fn print_tool_output_header(&self) {}
    fn update_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_summary(&self, _total_lines: usize) {}
    fn print_tool_timing(&self, _duration: &str, _tokens: u32, _context_pct: f32) {}
    fn print_tool_compact(
        &self,
        _tool_name: &str,
        _summary: &str,
        _duration: &str,
        _tokens: u32,
        _context_pct: f32,
    ) -> bool {
        false
    }
    fn print_todo_compact(&self, _content: Option<&str>, _is_write: bool) -> bool {
        false
    }
    fn print_tool_streaming_hint(&self, _tool_name: &str) {}
    fn print_tool_streaming_active(&self) {}

    fn print_agent_prompt(&self) {}

    fn print_agent_response(&self, response: &str) {
        self.responses.lock().unwrap().push(response.to_string());
    }

    fn flush(&self) {}
    fn finish_streaming_markdown(&self) {}
    fn reset_json_filter(&self) {}
    fn filter_json_tool_calls(&self, content: &str) -> String {
        content.to_string()
    }
    fn wants_full_output(&self) -> bool {
        false
    }
    fn notify_sse_received(&self) {}

    fn prompt_user_yes_no(&self, _message: &str) -> bool {
        false
    }

    fn prompt_user_choice(&self, _message: &str, _options: &[&str]) -> usize {
        0
    }
}

// =============================================================================
// Integration Tests
// =============================================================================

/// Test that all streaming chunks are processed before control returns.
/// This simulates the interactive mode flow where a user sends a message
/// and the agent processes the full response including multiple tool calls.
///
/// The key assertion is that BOTH tool calls from the first stream are
/// processed - if control returned after the first tool call, we'd only see 1.
#[tokio::test]
#[serial]
async fn test_streaming_processes_all_chunks_before_returning() {
    let temp_dir = TempDir::new().unwrap();
    std::env::set_current_dir(temp_dir.path()).unwrap();

    // Create mock provider
    let mock_provider = MockStreamingProvider::new();
    let first_stream_completed = mock_provider.first_stream_all_chunks_sent.clone();

    // Create provider registry with mock
    let mut registry = ProviderRegistry::new();
    registry.register(mock_provider);

    // Create tracking UI writer
    let tool_calls_seen = Arc::new(AtomicUsize::new(0));
    let ui_writer = TrackingUiWriter::new(tool_calls_seen.clone());

    // Create agent with mock provider
    let config = g3_config::Config::default();
    let mut agent = Agent::new_for_test(config, ui_writer.clone(), registry)
        .await
        .unwrap();

    // Execute a task - this should process ALL chunks before returning
    let result = agent.execute_task("test task", None, false).await;

    // The task may complete or error (due to auto-continue logic), but that's ok
    let _ = result;

    // CRITICAL ASSERTION 1: The first stream must have sent all its chunks
    assert!(
        first_stream_completed.load(Ordering::SeqCst),
        "First stream did not complete sending all chunks - control may have returned early"
    );

    // CRITICAL ASSERTION 2: Both tool calls from the first stream must have been processed
    let tool_count = ui_writer.tool_call_count();
    assert!(
        tool_count >= 2,
        "Expected at least 2 tool calls to be processed, but only {} were seen. \
         This indicates control was returned after the first tool call.",
        tool_count
    );
}

/// Test that the finished signal (chunk.finished = true) properly terminates
/// the stream processing loop.
#[tokio::test]
#[serial]
async fn test_finished_signal_terminates_stream() {
    let temp_dir = TempDir::new().unwrap();
    std::env::set_current_dir(temp_dir.path()).unwrap();

    // Create a simpler mock that just sends text and finishes
    struct SimpleFinishProvider {
        post_finish_chunk_processed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl LLMProvider for SimpleFinishProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            Ok(CompletionResponse {
                content: String::new(),
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
                },
                model: "simple".to_string(),
            })
        }

        async fn stream(&self, _request: CompletionRequest) -> Result<CompletionStream> {
            let (tx, rx) = mpsc::channel(32);
            let post_finish_flag = self.post_finish_chunk_processed.clone();

            tokio::spawn(async move {
                // Send some text
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: "Hello, this is a test response.".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                // Send finished signal
                let _ = tx
                    .send(Ok(CompletionChunk {
                        content: String::new(),
                        finished: true,
                        tool_calls: None,
                        usage: Some(Usage {
                            prompt_tokens: 10,
                            completion_tokens: 10,
                            total_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
                        }),
                        stop_reason: Some("end_turn".to_string()),
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await;

                // Wait a bit then send another chunk (should not be processed)
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

                // If this send succeeds and the receiver is still listening,
                // it means the stream wasn't properly terminated
                if tx
                    .send(Ok(CompletionChunk {
                        content: "THIS_SHOULD_NOT_APPEAR".to_string(),
                        finished: false,
                        tool_calls: None,
                        usage: None,
                        stop_reason: None,
                        tool_call_streaming: None,
                        upstream_ping: false,
                    }))
                    .await
                    .is_ok()
                {
                    // Channel still open - but this doesn't mean it was processed
                    // The flag is set if the content appears in responses
                }

                post_finish_flag.store(true, Ordering::SeqCst);
            });

            Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
        }

        fn name(&self) -> &str {
            "simple"
        }
        fn model(&self) -> &str {
            "simple-model"
        }
        fn has_native_tool_calling(&self) -> bool {
            false
        }
        fn supports_cache_control(&self) -> bool {
            false
        }
        fn max_tokens(&self) -> u32 {
            4096
        }
        fn temperature(&self) -> f32 {
            0.0
        }
    }

    let post_finish_flag = Arc::new(AtomicBool::new(false));
    let provider = SimpleFinishProvider {
        post_finish_chunk_processed: post_finish_flag.clone(),
    };

    let mut registry = ProviderRegistry::new();
    registry.register(provider);

    let tool_calls_seen = Arc::new(AtomicUsize::new(0));
    let ui_writer = TrackingUiWriter::new(tool_calls_seen);
    let config = g3_config::Config::default();
    let mut agent = Agent::new_for_test(config, ui_writer.clone(), registry)
        .await
        .unwrap();

    let result = agent.execute_task("test", None, false).await;

    assert!(result.is_ok(), "Task should complete successfully");

    // Verify the post-finish content was NOT processed
    let responses = ui_writer.responses();
    let all_responses = responses.join("");

    assert!(
        !all_responses.contains("THIS_SHOULD_NOT_APPEAR"),
        "Content after finished signal should not be processed. Got: {}",
        all_responses
    );
    assert!(
        all_responses.contains("Hello"),
        "Content before finished signal should be processed. Got: {}",
        all_responses
    );
}
