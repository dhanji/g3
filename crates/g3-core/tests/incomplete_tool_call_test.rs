//! Tests for the incomplete tool call detection feature

use g3_core::StreamingToolParser;
use g3_providers::CompletionChunk;

#[test]
fn test_has_incomplete_tool_call_empty_buffer() {
    let parser = StreamingToolParser::new();
    assert!(!parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_no_tool_pattern() {
    let mut parser = StreamingToolParser::new();
    let chunk = CompletionChunk {
        content: "Hello, I will help you with that.".to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    assert!(!parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_complete_tool_call() {
    let mut parser = StreamingToolParser::new();
    let chunk = CompletionChunk {
        content: r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Complete JSON should NOT be detected as incomplete
    assert!(!parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_truncated_tool_call() {
    let mut parser = StreamingToolParser::new();
    // Simulate truncated tool call - missing closing braces
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
    // Incomplete JSON should be detected
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_truncated_mid_value() {
    let mut parser = StreamingToolParser::new();
    // Simulate truncated tool call - cut off mid-value
    let chunk = CompletionChunk {
        content: r#"{"tool": "shell", "args": {"command": "cargo test --package g3-cli --test filter_json_test test_streaming -- --test-threads=1 2>&1 | tail"#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Incomplete JSON should be detected
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_with_text_before() {
    let mut parser = StreamingToolParser::new();
    // Text before the incomplete tool call
    let chunk = CompletionChunk {
        content: r#"Let me read that file for you.

{"tool": "read_file", "args": {"file_path":"#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Incomplete JSON should be detected
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_has_incomplete_tool_call_malformed_like_trace() {
    let mut parser = StreamingToolParser::new();
    // This simulates a truncated tool call where the stream ended mid-JSON
    // The actual trace showed truncated output, not malformed characters
    let chunk = CompletionChunk {
        content: r#"{"tool": "read_file", "args": {"file_path":"src/engine.rkt""#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Truncated JSON (missing closing braces) should be detected as incomplete
    assert!(parser.has_incomplete_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_empty_buffer() {
    let parser = StreamingToolParser::new();
    assert!(!parser.has_unexecuted_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_no_tool_pattern() {
    let mut parser = StreamingToolParser::new();
    let chunk = CompletionChunk {
        content: "Hello, I will help you with that.".to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    assert!(!parser.has_unexecuted_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_complete_tool_call() {
    let mut parser = StreamingToolParser::new();
    let chunk = CompletionChunk {
        content: r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Complete JSON tool call that wasn't executed should be detected
    assert!(parser.has_unexecuted_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_incomplete_json() {
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
    // Incomplete JSON should NOT be detected as unexecuted (it's incomplete, not unexecuted)
    assert!(!parser.has_unexecuted_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_with_trailing_text() {
    let mut parser = StreamingToolParser::new();
    // Complete JSON tool call followed by trailing text
    let chunk = CompletionChunk {
        content: r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}

Some trailing text after the JSON"#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Complete JSON tool call should be detected even with trailing text
    assert!(parser.has_unexecuted_tool_call());
}

#[test]
fn test_has_unexecuted_tool_call_with_text_before_and_after() {
    let mut parser = StreamingToolParser::new();
    let chunk = CompletionChunk {
        content: r#"Let me read that file.

{"tool": "shell", "args": {"command": "ls -la"}}

I'll execute this command now."#.to_string(),
        finished: false,
        tool_calls: None,
        usage: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    };
    parser.process_chunk(&chunk);
    // Complete JSON tool call should be detected
    assert!(parser.has_unexecuted_tool_call());
}
