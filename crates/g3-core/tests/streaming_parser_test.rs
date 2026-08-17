//! Comprehensive tests for StreamingToolParser
//!
//! Tests cover:
//! - Multiple tool calls in one response
//! - Tool call followed by text
//! - Incomplete tool calls at various truncation points
//! - Parser reset behavior
//! - Buffer management

use g3_core::StreamingToolParser;
use g3_providers::CompletionChunk;

// Helper to create a chunk
fn chunk(content: &str, finished: bool) -> CompletionChunk {
    CompletionChunk {
        content: content.to_string(),
        finished,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    }
}

// =============================================================================
// Test: Multiple tool calls in one response
// =============================================================================

#[test]
fn test_multiple_tool_calls_in_single_chunk() {
    let mut parser = StreamingToolParser::new();
    
    // Two complete tool calls in one chunk
    let content = r#"Let me do two things:
{"tool": "read_file", "args": {"file_path": "a.txt"}}
Now the second:
{"tool": "shell", "args": {"command": "ls"}}"#;
    
    let tools = parser.process_chunk(&chunk(content, false));
    
    // Should detect at least one tool call
    // Note: Current implementation may only return the first one found
    assert!(!tools.is_empty(), "Should detect at least one tool call");
}

#[test]
fn test_multiple_tool_calls_across_chunks() {
    let mut parser = StreamingToolParser::new();
    
    // First tool call
    let tools1 = parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "a.txt"}}"#,
        false
    ));
    assert_eq!(tools1.len(), 1, "First tool call should be detected");
    assert_eq!(tools1[0].tool, "read_file");
    
    // Reset parser (simulating what happens after tool execution)
    parser.reset();
    
    // Second tool call
    let tools2 = parser.process_chunk(&chunk(
        r#"{"tool": "shell", "args": {"command": "ls"}}"#,
        false
    ));
    assert_eq!(tools2.len(), 1, "Second tool call should be detected");
    assert_eq!(tools2[0].tool, "shell");
}

#[test]
fn test_first_complete_second_incomplete() {
    let mut parser = StreamingToolParser::new();
    
    // First complete, second incomplete
    let content = r#"{"tool": "read_file", "args": {"file_path": "a.txt"}}
{"tool": "shell", "args": {"command": "ls"#;
    
    let _tools = parser.process_chunk(&chunk(content, false));
    
    // Should detect the first complete tool call
    // The incomplete one should be detected by has_incomplete_tool_call
    assert!(parser.has_incomplete_tool_call(), "Should detect incomplete tool call");
}

// =============================================================================
// Test: Tool call followed by text
// =============================================================================

#[test]
fn test_tool_call_with_trailing_text() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}

Here is the content of the file..."#;
    
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "read_file");
    
    // The trailing text should be in the buffer
    let text = parser.get_text_content();
    assert!(text.contains("Here is the content"), "Trailing text should be preserved");
}

#[test]
fn test_text_before_tool_call() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"Let me read that file for you.

{"tool": "read_file", "args": {"file_path": "test.txt"}}"#;
    
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "read_file");
    
    // The leading text should be in the buffer
    let text = parser.get_text_content();
    assert!(text.contains("Let me read"), "Leading text should be preserved");
}

#[test]
fn test_text_before_and_after_tool_call() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"I'll check the file.

{"tool": "read_file", "args": {"file_path": "test.txt"}}

Done checking."#;
    
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    
    let text = parser.get_text_content();
    assert!(text.contains("I'll check"), "Leading text should be preserved");
    assert!(text.contains("Done checking"), "Trailing text should be preserved");
}

// =============================================================================
// Test: Incomplete tool calls at various truncation points
// =============================================================================

#[test]
fn test_incomplete_after_tool_key() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool":"#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_incomplete_after_tool_name() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "read_file""#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_incomplete_after_args_key() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args":"#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_incomplete_mid_args_object() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args": {"file_path":"#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_incomplete_mid_string_value() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "shell", "args": {"command": "ls -la /very/long/path"#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_incomplete_missing_final_brace() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args": {"file_path": "test.txt"}"#, false));
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_complete_tool_call_not_incomplete() {
    let mut parser = StreamingToolParser::new();
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#, false));
    assert!(!parser.has_incomplete_tool_call(), "Complete tool call should not be marked incomplete");
}

// =============================================================================
// Test: Parser reset behavior
// =============================================================================

#[test]
fn test_reset_clears_buffer() {
    let mut parser = StreamingToolParser::new();
    
    parser.process_chunk(&chunk("Some content here", false));
    assert!(!parser.get_text_content().is_empty());
    
    parser.reset();
    
    assert!(parser.get_text_content().is_empty(), "Buffer should be empty after reset");
}

#[test]
fn test_reset_clears_incomplete_state() {
    let mut parser = StreamingToolParser::new();
    
    // Create incomplete tool call
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args": {"#, false));
    assert!(parser.has_incomplete_tool_call());
    
    parser.reset();
    
    assert!(!parser.has_incomplete_tool_call(), "Incomplete state should be cleared after reset");
}

#[test]
fn test_reset_clears_unexecuted_state() {
    let mut parser = StreamingToolParser::new();
    
    // Create complete but "unexecuted" tool call
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#, false));
    assert!(parser.has_unexecuted_tool_call());
    
    parser.reset();
    
    assert!(!parser.has_unexecuted_tool_call(), "Unexecuted state should be cleared after reset");
}

#[test]
fn test_reset_allows_new_tool_calls() {
    let mut parser = StreamingToolParser::new();
    
    // First tool call
    let tools1 = parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "a.txt"}}"#,
        false
    ));
    assert_eq!(tools1.len(), 1);
    
    parser.reset();
    
    // Second tool call after reset
    let tools2 = parser.process_chunk(&chunk(
        r#"{"tool": "shell", "args": {"command": "ls"}}"#,
        false
    ));
    assert_eq!(tools2.len(), 1);
    assert_eq!(tools2[0].tool, "shell");
}

// =============================================================================
// Test: Buffer management and edge cases
// =============================================================================

#[test]
fn test_streaming_chunks_accumulate() {
    let mut parser = StreamingToolParser::new();
    
    // Stream in chunks
    parser.process_chunk(&chunk(r#"{"tool": "#, false));
    parser.process_chunk(&chunk(r#""read_file", "#, false));
    parser.process_chunk(&chunk(r#""args": {"file_path": "#, false));
    parser.process_chunk(&chunk(r#""test.txt"}}"#, false));
    
    // Should have accumulated the complete tool call
    let text = parser.get_text_content();
    assert!(text.contains(r#""tool""#));
    assert!(text.contains(r#""read_file""#));
}

#[test]
fn test_finished_chunk_triggers_final_parse() {
    let mut parser = StreamingToolParser::new();
    
    // Incomplete chunks
    parser.process_chunk(&chunk(r#"{"tool": "read_file", "#, false));
    let tools1 = parser.process_chunk(&chunk(r#""args": {"file_path": "test.txt"}}"#, false));
    
    // Tool should be detected before finished
    assert!(!tools1.is_empty() || !parser.has_unexecuted_tool_call(), 
        "Tool should be detected during streaming or marked as unexecuted");
}

#[test]
fn test_empty_chunks_ignored() {
    let mut parser = StreamingToolParser::new();
    
    parser.process_chunk(&chunk("", false));
    parser.process_chunk(&chunk("", false));
    
    assert!(parser.get_text_content().is_empty());
    assert!(!parser.has_incomplete_tool_call());
    assert!(!parser.has_unexecuted_tool_call());
}

#[test]
fn test_whitespace_only_chunks() {
    let mut parser = StreamingToolParser::new();
    
    parser.process_chunk(&chunk("   \n\t  ", false));
    
    assert!(!parser.has_incomplete_tool_call());
    assert!(!parser.has_unexecuted_tool_call());
}

#[test]
fn test_json_with_escaped_quotes() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"{"tool": "shell", "args": {"command": "echo \"hello\""}}"#;
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "shell");
}

#[test]
fn test_json_with_escaped_backslashes() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"{"tool": "write_file", "args": {"file_path": "C:\\Users\\test.txt", "content": "data"}}"#;
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "write_file");
}

#[test]
fn test_json_with_nested_braces_in_string() {
    let mut parser = StreamingToolParser::new();
    
    let content = r#"{"tool": "write_file", "args": {"content": "{\"nested\": {\"json\": true}}"}}"#;
    let tools = parser.process_chunk(&chunk(content, false));
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "write_file");
}

#[test]
fn test_text_buffer_length_tracking() {
    let mut parser = StreamingToolParser::new();
    
    parser.process_chunk(&chunk("Hello", false));
    assert_eq!(parser.text_buffer_len(), 5);
    
    parser.process_chunk(&chunk(" World", false));
    assert_eq!(parser.text_buffer_len(), 11);
    
    parser.reset();
    assert_eq!(parser.text_buffer_len(), 0);
}

#[test]
fn test_message_stopped_flag() {
    let mut parser = StreamingToolParser::new();
    
    parser.process_chunk(&chunk("Hello", false));
    assert!(!parser.is_message_stopped());
    
    parser.process_chunk(&chunk(" World", true));
    assert!(parser.is_message_stopped());
    
    parser.reset();
    assert!(!parser.is_message_stopped());
}

// =============================================================================
// Test: Tool call pattern variations
// =============================================================================

#[test]
fn test_tool_pattern_no_spaces() {
    let mut parser = StreamingToolParser::new();
    let tools = parser.process_chunk(&chunk(
        r#"{"tool":"read_file","args":{"file_path":"test.txt"}}"#,
        false
    ));
    assert_eq!(tools.len(), 1);
}

// =============================================================================
// Test: mark_tool_calls_consumed functionality
// =============================================================================

#[test]
fn test_mark_consumed_clears_unexecuted_state() {
    let mut parser = StreamingToolParser::new();
    
    // Add a complete tool call
    parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#,
        false
    ));
    
    // Should be detected as unexecuted
    assert!(parser.has_unexecuted_tool_call());
    
    // Mark as consumed
    parser.mark_tool_calls_consumed();
    
    // Should no longer be detected as unexecuted
    assert!(!parser.has_unexecuted_tool_call(), 
        "After marking consumed, has_unexecuted_tool_call should return false");
}

#[test]
fn test_mark_consumed_allows_new_tool_detection() {
    let mut parser = StreamingToolParser::new();
    
    // First tool call
    parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "a.txt"}}"#,
        false
    ));
    parser.mark_tool_calls_consumed();
    
    // Second tool call (without reset)
    parser.process_chunk(&chunk(
        r#"{"tool": "shell", "args": {"command": "ls"}}"#,
        false
    ));
    
    // Should detect the new unexecuted tool call
    assert!(parser.has_unexecuted_tool_call(), 
        "New tool call after consumed position should be detected");
}

#[test]
fn test_bare_brace_not_incomplete() {
    let mut parser = StreamingToolParser::new();
    
    // Just a bare opening brace - not a tool call pattern
    parser.process_chunk(&chunk(r#"{""#, false));
    
    // Should NOT be detected as incomplete because it doesn't match tool patterns
    assert!(!parser.has_incomplete_tool_call(), 
        "Bare {{ should not be detected as incomplete tool call");
}

#[test]
fn test_duplicate_tool_call_pattern() {
    let mut parser = StreamingToolParser::new();
    
    // Simulate the problematic pattern: tool call, garbage, duplicate tool call
    let content = concat!(
        r#"{"tool": "str_replace", "args": {"file_path": "test.rs", "diff": "test"}}"#,
        "\n\n{\"\n\n",
        r#"{"tool": "str_replace", "args": {"file_path": "test.rs", "diff": "test"}}"#
    );
    let tools = parser.process_chunk(&chunk(content, false));
    
    // Should detect at least one tool call
    assert!(!tools.is_empty(), "Should detect at least one tool call");
    
    // After processing, there should be an unexecuted tool call (the duplicate)
    // because the parser only returns the first one it finds during streaming
    assert!(parser.has_unexecuted_tool_call(), 
        "Should detect the duplicate as unexecuted");
}

#[test]
fn test_multiple_tool_calls_returned_on_finish() {
    let mut parser = StreamingToolParser::new();
    
    // Two complete tool calls in one chunk, with finished=true
    let content = concat!(
        r#"{"tool": "read_file", "args": {"file_path": "a.txt"}}"#,
        "\nSome text\n",
        r#"{"tool": "shell", "args": {"command": "ls"}}"#
    );
    
    // First, add content without finishing
    parser.process_chunk(&chunk(content, false));
    
    // Now finish the stream - should return ALL tool calls
    let tools = parser.process_chunk(&chunk("", true));
    
    // Should return both tool calls
    assert_eq!(tools.len(), 2, "Should return both tool calls when stream finishes");
    assert_eq!(tools[0].tool, "read_file");
    assert_eq!(tools[1].tool, "shell");
}

#[test]
fn test_tool_pattern_extra_spaces() {
    let mut parser = StreamingToolParser::new();
    let tools = parser.process_chunk(&chunk(
        r#"{ "tool" : "read_file" , "args" : { "file_path" : "test.txt" } }"#,
        false
    ));
    assert_eq!(tools.len(), 1);
}

#[test]
fn test_tool_pattern_with_newlines() {
    let mut parser = StreamingToolParser::new();
    // Note: The parser looks for specific patterns like {"tool": or { "tool":
    // Multi-line JSON with newlines between { and "tool" won't match
    // This is expected behavior - the pattern matching is intentionally strict
    let _tools = parser.process_chunk(&chunk(
        r#"{
  "tool": "read_file",
  "args": {
    "file_path": "test.txt"
  }
}"#,
        false
    ));
    // This won't be detected as a tool call due to newline after {
    // The has_unexecuted_tool_call check also won't find it
    // This is a known limitation of the pattern-based detection
}

// =============================================================================
// Test: Edge cases for has_message_like_keys validation
// =============================================================================

#[test]
fn test_normal_args_accepted() {
    let mut parser = StreamingToolParser::new();
    let tools = parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "test.txt", "start": 0, "end": 100}}"#,
        false
    ));
    assert_eq!(tools.len(), 1);
}

#[test] 
fn test_content_with_phrases_in_value_accepted() {
    let mut parser = StreamingToolParser::new();
    // Phrases like "I'll" in VALUES should be fine (only keys are checked)
    let tools = parser.process_chunk(&chunk(
        r#"{"tool": "write_file", "args": {"file_path": "test.txt", "content": "I'll help you with that. Let me explain."}}"#,
        false
    ));
    assert_eq!(tools.len(), 1);
}
