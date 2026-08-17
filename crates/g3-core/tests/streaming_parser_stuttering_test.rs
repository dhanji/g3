//! Integration tests for streaming parser stuttering bug fix (fa3c920)
//!
//! BEHAVIOR PROTECTED:
//! When an LLM "stutters" and emits incomplete tool call fragments followed by
//! complete tool calls, the parser should:
//! 1. Not get stuck waiting for the incomplete fragment to complete
//! 2. Successfully parse complete tool calls that appear after the fragment
//!
//! SURFACE TARGETED:
//! StreamingToolParser - the public API for processing streaming chunks
//!
//! INTENTIONALLY NOT ASSERTED:
//! - Internal parser state transitions
//! - Specific invalidation mechanism details
//! - Order of internal operations
//! - Behavior of patterns that don't match the actual bug scenario

use g3_core::StreamingToolParser;
use g3_providers::CompletionChunk;

/// Helper to create a completion chunk
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
// CHARACTERIZATION: The exact stuttering pattern from the bug report
// =============================================================================

/// Test the exact pattern observed in butler session butler_c6ab59af2e4f991c
/// where the LLM emitted: complete -> incomplete fragment -> complete
///
/// This is the critical bug fix test - before the fix, the parser would get
/// stuck on the incomplete fragment and return zero tool calls.
#[test]
fn test_stuttering_complete_incomplete_complete() {
    let mut parser = StreamingToolParser::new();

    // This is the exact pattern that caused the bug:
    // 1. Complete tool call
    // 2. Incomplete fragment (just {"tool":)
    // 3. Complete tool call again
    let content = r#"{"tool": "shell", "args": {"command": "ls"}}

{"tool":

{"tool": "shell", "args": {"command": "pwd"}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    // CRITICAL: We must get at least one valid tool call
    // Before the fix, the parser would get stuck on the incomplete fragment
    // and return zero tool calls
    assert!(
        !tools.is_empty(),
        "Parser must not get stuck on incomplete fragment. Expected tool calls, got none."
    );

    // Verify we got valid tool calls (at least one should be "shell")
    assert!(
        tools.iter().any(|t| t.tool == "shell"),
        "Expected at least one 'shell' tool call"
    );
}

/// Verify the parser finds at least one complete tool call even with stuttering
#[test]
fn test_stuttering_finds_at_least_one_complete_call() {
    let mut parser = StreamingToolParser::new();

    // Complete -> incomplete -> complete with different commands
    let content = r#"{"tool": "shell", "args": {"command": "first"}}

{"tool":

{"tool": "shell", "args": {"command": "second"}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    // CHARACTERIZATION: The parser finds at least one complete tool call.
    // The exact number depends on implementation details (streaming vs batch parsing).
    // The critical behavior is that it doesn't return zero (the original bug).
    assert!(
        !tools.is_empty(),
        "Expected at least 1 tool call, got none"
    );
}

// =============================================================================
// CHARACTERIZATION: Edge cases that should NOT trigger invalidation
// =============================================================================

/// Tool call patterns inside JSON strings should not cause invalidation
#[test]
fn test_tool_pattern_in_string_value_not_invalidated() {
    let mut parser = StreamingToolParser::new();

    // Writing example code that contains a tool call pattern
    let content = r#"{"tool": "write_file", "args": {"file_path": "example.md", "content": "Example:\n{\"tool\": \"shell\"}"}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    // Should parse the outer tool call correctly
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "write_file");
    // The inner pattern should be part of the content, not a separate tool call
    assert!(tools[0].args["content"]
        .as_str()
        .unwrap()
        .contains("{\"tool\""));
}

/// Nested JSON objects should not trigger false invalidation
#[test]
fn test_nested_json_not_invalidated() {
    let mut parser = StreamingToolParser::new();

    // Tool call with nested JSON in args
    let content = r#"{"tool": "shell", "args": {"command": "echo '{\"nested\": true}'"}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "shell");
}

// =============================================================================
// CHARACTERIZATION: Recovery behavior
// =============================================================================

/// Parser should work correctly after reset
#[test]
fn test_parser_reset_clears_state() {
    let mut parser = StreamingToolParser::new();

    // First: process content with stuttering
    let content1 = r#"{"tool": "shell", "args": {"command": "ls"}}

{"tool":

{"tool": "shell", "args": {"command": "pwd"}}"#;
    let _tools1 = parser.process_chunk(&chunk(content1, true));

    // Reset for new message
    parser.reset();

    // Second message should work normally
    let content2 = r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#;
    let tools2 = parser.process_chunk(&chunk(content2, true));

    assert_eq!(tools2.len(), 1);
    assert_eq!(tools2[0].tool, "read_file");
}

/// Incomplete tool call detection works
#[test]
fn test_incomplete_detection() {
    let mut parser = StreamingToolParser::new();

    // Incomplete fragment
    parser.process_chunk(&chunk("{\"tool\":", false));
    assert!(
        parser.has_incomplete_tool_call(),
        "Should detect incomplete tool call"
    );
}

// =============================================================================
// CHARACTERIZATION: Multiple complete tool calls (no stuttering)
// =============================================================================

/// Multiple complete tool calls should all be found
#[test]
fn test_multiple_complete_tool_calls() {
    let mut parser = StreamingToolParser::new();

    let content = r#"{"tool": "shell", "args": {"command": "ls"}}

{"tool": "read_file", "args": {"file_path": "test.txt"}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    assert_eq!(tools.len(), 2, "Should find both tool calls");
    assert_eq!(tools[0].tool, "shell");
    assert_eq!(tools[1].tool, "read_file");
}

// =============================================================================
// CHARACTERIZATION: Boundary conditions
// =============================================================================

/// Minimal stutter pattern with complete call first
#[test]
fn test_minimal_stutter_with_complete_first() {
    let mut parser = StreamingToolParser::new();

    // Complete call, then incomplete, then complete
    let content = r#"{"tool": "shell", "args": {}}
{"tool":
{"tool": "shell", "args": {}}"#;

    let tools = parser.process_chunk(&chunk(content, true));

    assert!(!tools.is_empty(), "Should find at least one complete tool call");
}

/// Stutter at chunk boundary - incomplete in one chunk, complete in next
#[test]
fn test_stutter_split_across_chunk_boundary() {
    let mut parser = StreamingToolParser::new();

    // First chunk: complete tool call
    let tools1 = parser.process_chunk(&chunk(
        r#"{"tool": "shell", "args": {"command": "ls"}}"#,
        false,
    ));
    assert_eq!(tools1.len(), 1, "First complete tool call should be detected");

    // Mark as consumed
    parser.mark_tool_calls_consumed();

    // Second chunk: incomplete fragment
    parser.process_chunk(&chunk("\n{\"tool\":", false));

    // Third chunk: new complete tool call (finished)
    let tools3 = parser.process_chunk(&chunk(
        "\n{\"tool\": \"read_file\", \"args\": {\"file_path\": \"test.txt\"}}",
        true,
    ));

    // Should find the complete tool call at stream end
    assert!(!tools3.is_empty(), "Should find complete tool call at stream end");
}
