//! Tool Result Poisoning Test
//!
//! This test reproduces the bug where partial JSON tool call patterns in tool results
//! (e.g., from reading a source file with comments containing tool call examples)
//! incorrectly trigger the parser's incomplete tool call detection.
//!
//! The key insight: tool results should NEVER be searched for JSON tool calls.
//! Only the LLM's actual response text should be parsed for tool calls.

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

/// Simulates a file that contains a partial tool call pattern in a comment.
/// This is the kind of content that would be returned by `shell` with `cat file.rs`.
const FILE_WITH_TOOL_CALL_COMMENT: &str = r#"//! Example module
//!
//! To call a tool, use this format:
//! {"tool": "shell", "args": {"command": "ls"}}
//!
//! Or for reading files:
//! {"tool": "read_file

fn main() {
    println!("Hello, world!");
}
"#;

/// REPRO: Tool result containing partial JSON tool call pattern should NOT
/// cause has_incomplete_tool_call() to return true.
///
/// Scenario:
/// 1. LLM calls shell tool: `cat file.rs`
/// 2. Tool returns file content containing `{"tool": "read_file` (incomplete JSON in comment)
/// 3. Tool result is added to context (as a User message, not through parser)
/// 4. LLM streams its next response: "I can see the file contains..."
/// 5. At end of stream, has_incomplete_tool_call() should be FALSE
///
/// The bug: has_incomplete_tool_call() was returning TRUE because it found
/// the partial pattern in... somewhere. Let's find out where.
#[test]
fn test_tool_result_with_partial_json_does_not_poison_parser() {
    let mut parser = StreamingToolParser::new();

    // Step 1: LLM streams a tool call
    let tools = parser.process_chunk(&chunk(
        r#"{"tool": "shell", "args": {"command": "cat file.rs"}}"#,
        true,
    ));
    assert_eq!(tools.len(), 1, "Should detect the shell tool call");
    assert_eq!(tools[0].tool, "shell");

    // Step 2: Tool executes and returns file content (this goes to context, not parser)
    // The tool result contains a partial JSON tool call pattern in a comment:
    // {"tool": "read_file
    // This is NOT fed to the parser - it goes to context_window.add_message()
    let _tool_result = FILE_WITH_TOOL_CALL_COMMENT;

    // Step 3: Parser is reset for next LLM response
    parser.reset();

    // Step 4: LLM streams its response after seeing the tool result
    // The LLM does NOT emit any tool calls, just commentary
    let tools = parser.process_chunk(&chunk(
        "I can see the file contains a main function that prints 'Hello, world!'.",
        false,
    ));
    assert!(tools.is_empty(), "No tool calls in this response");

    let tools = parser.process_chunk(&chunk(
        " The file also has some documentation comments with examples.",
        true,
    ));
    assert!(tools.is_empty(), "No tool calls in this response");

    // Step 5: CRITICAL - has_incomplete_tool_call should be FALSE
    // The parser should NOT think there's an incomplete tool call
    assert!(
        !parser.has_incomplete_tool_call(),
        "Parser should NOT detect incomplete tool call - tool results are not in parser buffer"
    );
    assert!(
        !parser.has_unexecuted_tool_call(),
        "Parser should NOT detect unexecuted tool call"
    );
}

/// REPRO: What if the LLM quotes the file content in its response?
/// This is the trickier case - the LLM might say:
/// "I see the file has a comment with {"tool": "read_file..."
///
/// Even in this case, the partial JSON is INLINE (not on its own line),
/// so it should be ignored by our line-boundary detection.
#[test]
fn test_llm_quoting_file_content_inline_does_not_poison() {
    let mut parser = StreamingToolParser::new();

    // LLM quotes the file content inline in its response
    let tools = parser.process_chunk(&chunk(
        r#"I see the file has a comment showing the format: {"tool": "read_file"#,
        false,
    ));
    assert!(tools.is_empty(), "Inline quote should not trigger tool detection");

    let tools = parser.process_chunk(&chunk(
        " which is incomplete in the example.",
        true,
    ));
    assert!(tools.is_empty(), "No tool calls");

    // Should NOT detect incomplete tool call - the pattern was inline
    assert!(
        !parser.has_incomplete_tool_call(),
        "Inline quoted pattern should NOT be detected as incomplete tool call"
    );
}

/// REPRO: What if the LLM quotes the file content on its own line?
/// This is the problematic case:
/// 
/// "The file contains:\n{"tool": "read_file\n"
///
/// The pattern IS on its own line, but it's clearly not a real tool call -
/// it's the LLM quoting file content.
#[test]
fn test_llm_quoting_file_content_on_own_line_should_not_poison() {
    let mut parser = StreamingToolParser::new();

    // LLM quotes the file content, putting the example on its own line
    let tools = parser.process_chunk(&chunk(
        "The file contains this example:\n",
        false,
    ));
    assert!(tools.is_empty());

    // This is the problematic chunk - partial JSON on its own line
    let tools = parser.process_chunk(&chunk(
        r#"{"tool": "read_file"#,
        false,
    ));
    // Currently this might incorrectly set in_json_tool_call = true
    // But it should NOT return a tool call since JSON is incomplete
    assert!(tools.is_empty(), "Incomplete JSON should not return tool call");

    // More content follows that makes it clear this isn't a real tool call
    let tools = parser.process_chunk(&chunk(
        "\nwhich shows how to read files.",
        true,
    ));
    assert!(tools.is_empty());

    // THE BUG: This currently returns true because the parser found
    // {"tool": "read_file on its own line and the JSON never completed.
    // 
    // The fix: When we see content AFTER the partial JSON that isn't valid
    // JSON continuation (like a newline followed by regular text), we should
    // clear the in_json_tool_call state.
    assert!(
        !parser.has_incomplete_tool_call(),
        "Quoted example followed by regular text should NOT be detected as incomplete tool call"
    );
}

/// Test that a REAL incomplete tool call IS detected.
/// This is the legitimate case we want to catch.
#[test]
fn test_real_incomplete_tool_call_is_detected() {
    let mut parser = StreamingToolParser::new();

    // LLM starts emitting a real tool call but stream gets cut off
    let tools = parser.process_chunk(&chunk(
        "Let me check that file.\n",
        false,
    ));
    assert!(tools.is_empty());

    let tools = parser.process_chunk(&chunk(
        r#"{"tool": "read_file", "args": {"file_path": "src/main"#,
        true, // Stream ends abruptly!
    ));
    // JSON is incomplete, so no tool call returned
    assert!(tools.is_empty());

    // This IS a real incomplete tool call - it's valid JSON so far,
    // just cut off mid-stream
    assert!(
        parser.has_incomplete_tool_call(),
        "Real incomplete tool call SHOULD be detected"
    );
}

/// REPRO: LLM quotes file content that has a comment with partial tool call.
/// The pattern is NOT on its own line because it's prefixed with `//`.
///
/// Example: "//! {"tool": "read_file"
///
/// This should be ignored because `//` precedes the pattern on the same line.
#[test]
fn test_llm_quoting_comment_with_partial_tool_call() {
    let mut parser = StreamingToolParser::new();

    // LLM shows the file content including a comment line
    let tools = parser.process_chunk(&chunk(
        "The file has this documentation:\n",
        false,
    ));
    assert!(tools.is_empty());

    // Comment line with partial tool call - NOT on its own line due to `//!` prefix
    let tools = parser.process_chunk(&chunk(
        r#"//! {"tool": "read_file"#,
        false,
    ));
    assert!(tools.is_empty(), "Comment prefix means pattern is not on its own line");

    let tools = parser.process_chunk(&chunk(
        "\nAnd more documentation follows.",
        true,
    ));
    assert!(tools.is_empty());

    // Should NOT detect incomplete tool call - pattern was not on its own line
    assert!(
        !parser.has_incomplete_tool_call(),
        "Pattern with // prefix should NOT be detected as incomplete tool call"
    );
}

/// REPRO: Multiple comment styles that should all be ignored.
#[test]
fn test_various_comment_prefixes_with_partial_tool_calls() {
    let test_cases = [
        ("// {\"tool\": \"shell\"", "C-style line comment"),
        ("//! {\"tool\": \"shell\"", "Rust doc comment"),
        ("/// {\"tool\": \"shell\"", "Rust doc comment alt"),
        ("# {\"tool\": \"shell\"", "Python/shell comment"),
        ("-- {\"tool\": \"shell\"", "SQL comment"),
        ("* {\"tool\": \"shell\"", "Block comment continuation"),
    ];

    for (input, description) in test_cases {
        let mut parser = StreamingToolParser::new();
        
        let tools = parser.process_chunk(&chunk(input, true));
        assert!(tools.is_empty(), "{}: should not detect tool call", description);
        
        assert!(
            !parser.has_incomplete_tool_call(),
            "{}: should NOT report incomplete tool call",
            description
        );
    }
}
