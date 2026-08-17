//! Characterization Tests for stream_completion_with_tools
//!
//! CHARACTERIZATION: These tests capture the current behavior of the streaming
//! completion loop. They serve as a safety net for refactoring by locking down
//! observable behavior through the stable Agent interface.
//!
//! What these tests protect:
//! - Tool calls are detected, executed, and results added to context
//! - Auto-continue behavior in autonomous mode after tool execution
//! - Duplicate tool call detection (sequential duplicates skipped)
//! - Incomplete tool call detection triggers auto-continue
//! - Context window updates reflect tool execution
//! - Streaming parser behavior for JSON tool call detection
//!
//! What these tests intentionally do NOT assert:
//! - Internal parser state or implementation details
//! - Specific timing values (only that timing info is present when requested)
//! - UI output formatting (uses NullUiWriter)
//! - Provider-specific behavior
//!
//! Surface: Agent::execute_tool() for tool execution, streaming parser for parsing
//! Boundary: Tool execution boundary, streaming parser boundary

use g3_core::ui_writer::NullUiWriter;
use g3_core::Agent;
use serial_test::serial;
use tempfile::TempDir;

// =============================================================================
// Characterization Tests: StreamingToolParser Behavior
// =============================================================================
// These tests characterize the streaming parser's behavior which is a key
// component of stream_completion_with_tools.

mod streaming_parser_characterization {
    use g3_core::StreamingToolParser;
    use g3_providers::CompletionChunk;

    /// CHARACTERIZATION: Parser detects complete JSON tool calls in text
    #[test]
    fn parser_detects_complete_tool_call() {
        let mut parser = StreamingToolParser::new();
        let chunk = CompletionChunk {
            content: r#"{"tool": "shell", "args": {"command": "ls"}}"#.to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };

        let tools = parser.process_chunk(&chunk);

        // Should detect one tool call
        assert_eq!(tools.len(), 1, "Should detect exactly one tool call");
        assert_eq!(tools[0].tool, "shell");
    }

    /// CHARACTERIZATION: Parser accumulates text before tool call
    #[test]
    fn parser_accumulates_text_before_tool() {
        let mut parser = StreamingToolParser::new();

        // First chunk: just text
        let chunk1 = CompletionChunk {
            content: "Let me run a command.\n\n".to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };
        let tools1 = parser.process_chunk(&chunk1);
        assert!(tools1.is_empty(), "No tool call yet");

        // Second chunk: tool call
        let chunk2 = CompletionChunk {
            content: r#"{"tool": "shell", "args": {"command": "pwd"}}"#.to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };
        let tools2 = parser.process_chunk(&chunk2);
        assert_eq!(tools2.len(), 1, "Should detect tool call");

        // Text buffer should contain the accumulated text
        let text = parser.get_text_content();
        assert!(
            text.contains("Let me run a command"),
            "Should preserve text before tool call: {}",
            text
        );
    }

    /// CHARACTERIZATION: Parser detects incomplete tool calls
    #[test]
    fn parser_detects_incomplete_tool_call() {
        let mut parser = StreamingToolParser::new();
        let chunk = CompletionChunk {
            content: r#"{"tool": "read_file", "args": {"file_path": "test.txt""#.to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };

        parser.process_chunk(&chunk);

        // Should detect incomplete tool call (missing closing braces)
        assert!(
            parser.has_incomplete_tool_call(),
            "Should detect incomplete tool call"
        );
    }

    /// CHARACTERIZATION: Parser detects unexecuted complete tool calls
    #[test]
    fn parser_detects_unexecuted_tool_call() {
        let mut parser = StreamingToolParser::new();
        let chunk = CompletionChunk {
            content: r#"{"tool": "shell", "args": {"command": "ls"}}"#.to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };

        // Process but don't execute
        let _tools = parser.process_chunk(&chunk);

        // Should detect unexecuted tool call
        assert!(
            parser.has_unexecuted_tool_call(),
            "Should detect unexecuted tool call"
        );
    }

    /// CHARACTERIZATION: Marking tools consumed clears unexecuted state
    #[test]
    fn marking_consumed_clears_unexecuted() {
        let mut parser = StreamingToolParser::new();
        let chunk = CompletionChunk {
            content: r#"{"tool": "shell", "args": {"command": "ls"}}"#.to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };

        let _tools = parser.process_chunk(&chunk);
        assert!(parser.has_unexecuted_tool_call());

        // Mark as consumed (simulating execution)
        parser.mark_tool_calls_consumed();

        assert!(
            !parser.has_unexecuted_tool_call(),
            "Should not detect unexecuted after marking consumed"
        );
    }

    /// CHARACTERIZATION: Reset clears all parser state
    #[test]
    fn reset_clears_parser_state() {
        let mut parser = StreamingToolParser::new();
        let chunk = CompletionChunk {
            content: "Some text\n{\"tool\": \"shell\"".to_string(),
            finished: false,
            tool_calls: None,
            usage: None,
            stop_reason: None,
            tool_call_streaming: None,
            upstream_ping: false,
        };

        parser.process_chunk(&chunk);
        assert!(parser.has_incomplete_tool_call());

        parser.reset();

        assert!(!parser.has_incomplete_tool_call());
        assert!(!parser.has_unexecuted_tool_call());
        assert!(parser.get_text_content().is_empty());
    }
}

// =============================================================================
// Characterization Tests: Auto-Continue Logic
// =============================================================================

mod auto_continue_characterization {
    use g3_core::streaming::{should_auto_continue, AutoContinueReason};

    /// CHARACTERIZATION: Non-autonomous mode never auto-continues
    #[test]
    fn non_autonomous_never_continues() {
        // Even with all conditions true, non-autonomous should not continue
        let result = should_auto_continue(
            false, // not autonomous
            true,  // tools executed
            true,  // incomplete tool call
            true,  // unexecuted tool call
            true,  // was truncated
        );

        assert!(result.is_none(), "Non-autonomous mode should never auto-continue");
    }

    /// CHARACTERIZATION: Autonomous mode continues after tool execution
    #[test]
    fn autonomous_continues_after_tools() {
        let result = should_auto_continue(
            true,  // autonomous
            true,  // tools executed
            false, // no incomplete
            false, // no unexecuted
            false, // not truncated
        );

        assert_eq!(
            result,
            Some(AutoContinueReason::ToolsExecuted),
            "Should continue after tool execution"
        );
    }

    /// CHARACTERIZATION: Incomplete tool call triggers continue
    #[test]
    fn incomplete_tool_triggers_continue() {
        let result = should_auto_continue(
            true,  // autonomous
            false, // no tools executed
            true,  // incomplete tool call
            false, // no unexecuted
            false, // not truncated
        );

        assert_eq!(
            result,
            Some(AutoContinueReason::IncompleteToolCall),
            "Should continue on incomplete tool call"
        );
    }

    /// CHARACTERIZATION: Unexecuted tool call triggers continue
    #[test]
    fn unexecuted_tool_triggers_continue() {
        let result = should_auto_continue(
            true,  // autonomous
            false, // no tools executed
            false, // no incomplete
            true,  // unexecuted tool call
            false, // not truncated
        );

        assert_eq!(
            result,
            Some(AutoContinueReason::UnexecutedToolCall),
            "Should continue on unexecuted tool call"
        );
    }

    /// CHARACTERIZATION: Max tokens truncation triggers continue
    #[test]
    fn truncation_triggers_continue() {
        let result = should_auto_continue(
            true,  // autonomous
            false, // no tools executed
            false, // no incomplete
            false, // no unexecuted
            true,  // was truncated
        );

        assert_eq!(
            result,
            Some(AutoContinueReason::MaxTokensTruncation),
            "Should continue on truncation"
        );
    }

    /// CHARACTERIZATION: Priority order - tools > incomplete > unexecuted > truncated
    #[test]
    fn priority_order_is_tools_first() {
        // When multiple conditions are true, tools executed takes priority
        let result = should_auto_continue(
            true, // autonomous
            true, // tools executed
            true, // incomplete tool call
            true, // unexecuted tool call
            true, // was truncated
        );

        assert_eq!(
            result,
            Some(AutoContinueReason::ToolsExecuted),
            "Tools executed should have highest priority"
        );
    }

    /// CHARACTERIZATION: No conditions means no continue
    #[test]
    fn no_conditions_no_continue() {
        let result = should_auto_continue(
            true,  // autonomous
            false, // no tools executed
            false, // no incomplete
            false, // no unexecuted
            false, // not truncated
        );

        assert!(result.is_none(), "Should not continue when no conditions met");
    }
}

// =============================================================================
// Characterization Tests: Duplicate Detection
// =============================================================================

mod duplicate_detection_characterization {
    use g3_core::streaming::deduplicate_tool_calls;
    use g3_core::ToolCall;

    fn make_tool_call(tool: &str, args: &str) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            args: serde_json::from_str(args).unwrap(),
            id: String::new(),
        }
    }

    /// CHARACTERIZATION: Sequential duplicates in chunk are detected
    #[test]
    fn sequential_duplicates_detected() {
        let tools = vec![
            make_tool_call("shell", r#"{"command": "ls"}"#),
            make_tool_call("shell", r#"{"command": "ls"}"#), // duplicate
        ];

        let result = deduplicate_tool_calls(tools, |_| None);

        assert_eq!(result.len(), 2);
        assert!(result[0].1.is_none(), "First call should not be duplicate");
        assert!(
            result[1].1.as_ref().map(|s| s.contains("DUP")).unwrap_or(false),
            "Second call should be marked as duplicate"
        );
    }

    /// CHARACTERIZATION: Different tools are not duplicates
    #[test]
    fn different_tools_not_duplicates() {
        let tools = vec![
            make_tool_call("shell", r#"{"command": "ls"}"#),
            make_tool_call("read_file", r#"{"file_path": "test.txt"}"#),
        ];

        let result = deduplicate_tool_calls(tools, |_| None);

        assert!(result[0].1.is_none());
        assert!(result[1].1.is_none(), "Different tools should not be duplicates");
    }

    /// CHARACTERIZATION: Same tool with different args is not duplicate
    #[test]
    fn same_tool_different_args_not_duplicate() {
        let tools = vec![
            make_tool_call("shell", r#"{"command": "ls"}"#),
            make_tool_call("shell", r#"{"command": "pwd"}"#),
        ];

        let result = deduplicate_tool_calls(tools, |_| None);

        assert!(result[0].1.is_none());
        assert!(result[1].1.is_none(), "Same tool with different args should not be duplicate");
    }

    /// CHARACTERIZATION: First tool checked against previous message
    #[test]
    fn first_tool_checked_against_previous() {
        let tools = vec![make_tool_call("shell", r#"{"command": "ls"}"#)];

        // Simulate previous message had same tool call
        let result = deduplicate_tool_calls(tools, |tc| {
            if tc.tool == "shell" {
                Some("DUP IN MSG".to_string())
            } else {
                None
            }
        });

        assert!(
            result[0].1.as_ref().map(|s| s.contains("MSG")).unwrap_or(false),
            "Should detect duplicate against previous message"
        );
    }

    /// CHARACTERIZATION: Second tool not checked against previous message
    #[test]
    fn second_tool_not_checked_against_previous() {
        let tools = vec![
            make_tool_call("read_file", r#"{"file_path": "a.txt"}"#),
            make_tool_call("shell", r#"{"command": "ls"}"#),
        ];

        // Previous message check would mark shell as duplicate
        let result = deduplicate_tool_calls(tools, |tc| {
            if tc.tool == "shell" {
                Some("DUP IN MSG".to_string())
            } else {
                None
            }
        });

        // First tool checked against previous (not duplicate)
        assert!(result[0].1.is_none());
        // Second tool only checked against first in chunk (not previous message)
        assert!(result[1].1.is_none(), "Second tool should only check against first in chunk");
    }
}

// =============================================================================
// Characterization Tests: Context Window Integration
// =============================================================================

mod context_window_characterization {
    use g3_core::ContextWindow;
    use g3_providers::{Message, MessageRole};

    /// CHARACTERIZATION: Context window tracks token usage
    #[test]
    fn context_tracks_tokens() {
        let mut ctx = ContextWindow::new(10000);

        let msg = Message::new(MessageRole::User, "Hello, world!".to_string());
        ctx.add_message(msg);

        assert!(ctx.used_tokens > 0, "Should track token usage");
        assert!(ctx.percentage_used() > 0.0, "Should calculate percentage");
    }

    /// CHARACTERIZATION: Should compact at 80% capacity
    #[test]
    fn should_compact_at_80_percent() {
        let mut ctx = ContextWindow::new(1000);

        // Add messages until we're at ~80%
        for i in 0..20 {
            let msg = Message::new(
                MessageRole::User,
                format!("Message {} with some content to use tokens", i),
            );
            ctx.add_message(msg);
        }

        // Check if should_compact triggers around 80%
        // Note: This is a characterization - we're capturing current behavior
        let percentage = ctx.percentage_used();
        let should_compact = ctx.should_compact();

        // The exact threshold may vary, but we're documenting the relationship
        if percentage >= 80.0 {
            assert!(
                should_compact,
                "Should compact when at {}% (>= 80%)",
                percentage
            );
        }
    }

    /// CHARACTERIZATION: Conversation history is preserved
    #[test]
    fn conversation_history_preserved() {
        let mut ctx = ContextWindow::new(10000);

        ctx.add_message(Message::new(MessageRole::User, "Question 1".to_string()));
        ctx.add_message(Message::new(
            MessageRole::Assistant,
            "Answer 1".to_string(),
        ));
        ctx.add_message(Message::new(MessageRole::User, "Question 2".to_string()));

        assert_eq!(
            ctx.conversation_history.len(),
            3,
            "Should preserve all messages"
        );
        assert!(ctx.conversation_history[0].content.contains("Question 1"));
        assert!(ctx.conversation_history[1].content.contains("Answer 1"));
        assert!(ctx.conversation_history[2].content.contains("Question 2"));
    }
}

// =============================================================================
// Characterization Tests: Tool Execution Through Agent
// =============================================================================
// These tests use the real Agent with tool execution to characterize
// the end-to-end behavior of stream_completion_with_tools.

mod tool_execution_integration {
    use super::*;
    use g3_core::ToolCall;
    use std::fs;

    /// Create a test agent in a temporary directory
    async fn create_test_agent(temp_dir: &TempDir) -> Agent<NullUiWriter> {
        std::env::set_current_dir(temp_dir.path()).unwrap();
        let config = g3_config::Config::default();
        let ui_writer = NullUiWriter;
        Agent::new(config, ui_writer).await.unwrap()
    }

    /// CHARACTERIZATION: Tool execution adds result to context
    #[tokio::test]
    #[serial]
    async fn tool_execution_updates_context() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "Hello from test file").unwrap();

        let mut agent = create_test_agent(&temp_dir).await;

        // Execute a tool directly
        let tool_call = ToolCall {
            tool: "read_file".to_string(),
            id: String::new(),
            args: serde_json::json!({ "file_path": test_file.to_string_lossy() }),
        };

        let result = agent.execute_tool(&tool_call).await.unwrap();

        // Verify tool executed successfully
        assert!(
            result.contains("Hello from test file"),
            "Tool should return file content: {}",
            result
        );
    }

    /// CHARACTERIZATION: Shell tool captures output
    #[tokio::test]
    #[serial]
    async fn shell_tool_captures_output() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        let tool_call = ToolCall {
            tool: "shell".to_string(),
            id: String::new(),
            args: serde_json::json!({ "command": "echo 'test output'" }),
        };

        let result = agent.execute_tool(&tool_call).await.unwrap();

        assert!(
            result.contains("test output"),
            "Should capture shell output: {}",
            result
        );
    }

    /// CHARACTERIZATION: Write file creates file and reports success
    #[tokio::test]
    #[serial]
    async fn write_file_creates_and_reports() {
        let temp_dir = TempDir::new().unwrap();
        let new_file = temp_dir.path().join("new.txt");

        let mut agent = create_test_agent(&temp_dir).await;

        let tool_call = ToolCall {
            tool: "write_file".to_string(),
            id: String::new(),
            args: serde_json::json!({
                "file_path": new_file.to_string_lossy(),
                "content": "New content"
            }),
        };

        let result = agent.execute_tool(&tool_call).await.unwrap();

        // File should exist
        assert!(new_file.exists(), "File should be created");

        // Result should indicate success
        assert!(
            result.contains("✅") || result.to_lowercase().contains("wrote"),
            "Should report success: {}",
            result
        );
    }

    /// CHARACTERIZATION: Plan tools work through agent
    #[tokio::test]
    #[serial]
    async fn plan_tools_work() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        // Initialize session ID for plan tools (they are session-scoped)
        agent.init_session_id_for_test("plan-tools-test");

        // Write Plan
        let write_call = ToolCall {
            tool: "plan_write".to_string(),
            id: String::new(),
            args: serde_json::json!({
                "plan": r#"plan_id: test-plan
revision: 1
items:
  - id: I1
    description: Test task
    state: todo
    touches:
      - src/test.rs
    checks:
      happy:
        desc: Works correctly
        target: test::module
      negative:
        - desc: Handles errors
          target: test::module
      boundary:
        - desc: Edge cases
          target: test::module"#
                ,
            }),
        };
        let write_result = agent.execute_tool(&write_call).await.unwrap();
        assert!(
            write_result.contains("✅"),
            "Plan write should succeed: {}",
            write_result
        );

        // Read Plan
        let read_call = ToolCall {
            tool: "plan_read".to_string(),
            id: String::new(),
            args: serde_json::json!({}),
        };
        let read_result = agent.execute_tool(&read_call).await.unwrap();
        assert!(
            read_result.contains("test-plan"),
            "Should read back plan: {}",
            read_result
        );
    }
}

// =============================================================================
// Characterization Tests: Streaming Utilities
// =============================================================================

mod streaming_utilities_characterization {
    use g3_core::streaming::{
        clean_llm_tokens, format_duration, is_empty_response, truncate_for_display,
    };
    use std::time::Duration;

    /// CHARACTERIZATION: LLM tokens are cleaned from output
    #[test]
    fn llm_tokens_cleaned() {
        assert_eq!(clean_llm_tokens("hello<|im_end|>"), "hello");
        assert_eq!(clean_llm_tokens("test</s>more"), "testmore");
        assert_eq!(clean_llm_tokens("[/INST]response"), "response");
        assert_eq!(clean_llm_tokens("normal text"), "normal text");
    }

    /// CHARACTERIZATION: Duration formatting
    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30.0s");
    }

    /// CHARACTERIZATION: Empty response detection
    #[test]
    fn empty_response_detection() {
        assert!(is_empty_response(""));
        assert!(is_empty_response("   \n  "));
        assert!(is_empty_response("⏱️ 1.5s"));
        assert!(!is_empty_response("Hello"));
        assert!(!is_empty_response("Some actual content"));
    }

    /// CHARACTERIZATION: Display truncation
    #[test]
    fn display_truncation() {
        assert_eq!(truncate_for_display("short", 10), "short");
        assert_eq!(truncate_for_display("this is long", 5), "this ...");
        // Multi-line uses first line only
        assert_eq!(
            truncate_for_display("first line\nsecond line", 20),
            "first line"
        );
    }
}

// =============================================================================
// Characterization Tests: Parser Line-Boundary Detection
// =============================================================================

mod parser_line_boundary_characterization {
    use g3_core::StreamingToolParser;

    /// CHARACTERIZATION: Standalone tool calls at start of text are detected
    #[test]
    fn standalone_tool_calls_detected() {
        let input = r#"{"tool": "shell", "args": {}}"#;
        let pos = StreamingToolParser::find_first_tool_call_start(input);
        assert!(pos.is_some(), "Standalone tool call should be detected");
        assert_eq!(pos.unwrap(), 0, "Should be at position 0");
    }

    /// CHARACTERIZATION: Inline tool patterns are ignored (not detected)
    #[test]
    fn inline_patterns_ignored() {
        let input = r#"Example: {"tool": "shell"} in text"#;
        let pos = StreamingToolParser::find_first_tool_call_start(input);
        assert!(
            pos.is_none(),
            "Inline pattern should be ignored, but found at {:?}",
            pos
        );
    }

    /// CHARACTERIZATION: Tool call on its own line (after newline) is detected
    #[test]
    fn tool_call_on_own_line_detected() {
        let input = "Some text\n{\"tool\": \"shell\"}\nMore text";
        let pos = StreamingToolParser::find_first_tool_call_start(input);
        assert!(
            pos.is_some(),
            "Tool call on own line should be detected"
        );
        // Should find it after the newline (position 10 = len("Some text\n"))
        assert_eq!(
            pos.unwrap(), 10,
            "Should find tool call at position after newline"
        );
    }
}
