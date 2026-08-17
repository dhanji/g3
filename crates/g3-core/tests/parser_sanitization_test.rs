//! Parser Line-Boundary Detection Tests
//!
//! CHARACTERIZATION: These tests verify that tool call patterns are only detected
//! when they appear on their own line (at start of text or after a newline with
//! only whitespace before the pattern).
//!
//! What these tests protect:
//! - Tool call patterns in various contexts (code blocks, quotes, etc.) are IGNORED
//! - Tool calls on their own line are DETECTED
//! - Edge cases at line boundaries
//! - Unicode handling
//!
//! What these tests intentionally do NOT assert:
//! - Internal parser state
//! - Exact detection implementation
//!
//! Related commits:
//! - Original: 4c36cc0: fix: prevent parser poisoning from inline tool-call JSON patterns
//! - Updated: Line-boundary detection instead of sanitization

use g3_core::StreamingToolParser;

// =============================================================================
// Test: Code block contexts
// =============================================================================

mod code_block_contexts {
    use super::*;

    /// Test tool pattern in markdown inline code - should be IGNORED
    #[test]
    fn test_inline_code_backticks() {
        let input = "Use `{\"tool\": \"shell\"}` to run commands";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Should be ignored since it's inline
        assert!(result.is_none(), "Inline code should be ignored");
    }

    /// Test tool pattern after code fence (should be DETECTED - it's on its own line)
    #[test]
    fn test_after_code_fence_standalone() {
        // Tool call on its own line after a code fence marker
        let input = "```\n{\"tool\": \"shell\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // The tool call is on its own line, should be detected
        assert!(result.is_some(), "Standalone after fence should be detected");
        assert_eq!(result.unwrap(), 4, "Should be at position 4 (after ```\\n)");
    }

    /// Test tool pattern in prose explanation - should be IGNORED
    #[test]
    fn test_prose_explanation() {
        let input = "The format is {\"tool\": \"name\", \"args\": {...}} where name is the tool";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        assert!(result.is_none(), "Prose should be ignored");
    }
}

// =============================================================================
// Test: Line boundary edge cases
// =============================================================================

mod line_boundary_cases {
    use super::*;

    /// Test empty lines don't affect detection
    #[test]
    fn test_empty_lines_before_tool_call() {
        let input = "\n\n{\"tool\": \"shell\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Tool call is on its own line (after empty lines), should be detected
        assert!(result.is_some(), "Standalone after empty lines should be detected");
        assert_eq!(result.unwrap(), 2, "Should be at position 2 (after two newlines)");
    }

    /// Test whitespace-only lines
    #[test]
    fn test_whitespace_only_lines() {
        let input = "   \n  \n{\"tool\": \"shell\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Tool call is on its own line, should be detected
        assert!(result.is_some(), "Standalone after whitespace lines should be detected");
    }

    /// Test tool call with leading whitespace (indented)
    #[test]
    fn test_indented_tool_call() {
        let input = "    {\"tool\": \"shell\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Indented but on its own line, should be detected
        assert!(result.is_some(), "Indented standalone should be detected");
        assert_eq!(result.unwrap(), 4, "Should be at position 4 (after 4 spaces)");
    }

    /// Test tool call with tabs
    #[test]
    fn test_tab_indented_tool_call() {
        let input = "\t{\"tool\": \"shell\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Tab-indented but on its own line, should be detected
        assert!(result.is_some(), "Tab-indented standalone should be detected");
        assert_eq!(result.unwrap(), 1, "Should be at position 1 (after tab)");
    }
}

// =============================================================================
// Test: Special characters and Unicode
// =============================================================================

mod unicode_handling {
    use super::*;

    /// Test tool pattern after emoji - should be IGNORED (inline)
    #[test]
    fn test_after_emoji() {
        let input = "🔧 {\"tool\": \"shell\"}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Emoji before means it's inline, should be ignored
        assert!(result.is_none(), "After emoji should be ignored");
    }

    /// Test tool pattern after bullet point - should be IGNORED (inline)
    #[test]
    fn test_after_bullet() {
        let input = "• {\"tool\": \"shell\"}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Bullet before means it's inline, should be ignored
        assert!(result.is_none(), "After bullet should be ignored");
    }

    /// Test tool pattern after CJK text - should be IGNORED (inline)
    #[test]
    fn test_after_cjk() {
        let input = "使用 {\"tool\": \"shell\"} 命令";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // CJK text before means it's inline, should be ignored
        assert!(result.is_none(), "After CJK should be ignored");
    }

    /// Test tool pattern with Unicode in args on its own line - should be DETECTED
    #[test]
    fn test_unicode_in_args_standalone() {
        let input = "{\"tool\": \"shell\", \"args\": {\"command\": \"echo 你好\"}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Standalone, should be detected
        assert!(result.is_some(), "Unicode in args standalone should be detected");
        assert_eq!(result.unwrap(), 0, "Should be at position 0");
    }

    /// Test tool pattern with Unicode in args inline - should be IGNORED
    #[test]
    fn test_unicode_in_args_inline() {
        let input = "Example: {\"tool\": \"shell\", \"args\": {\"command\": \"echo 你好\"}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Inline, should be ignored
        assert!(result.is_none(), "Unicode in args inline should be ignored");
    }
}

// =============================================================================
// Test: Multiple patterns on same line
// =============================================================================

mod multiple_patterns {
    use super::*;

    /// Test three tool patterns on one line - all should be IGNORED
    #[test]
    fn test_three_patterns() {
        let input = "Compare {\"tool\": \"a\"} vs {\"tool\": \"b\"} vs {\"tool\": \"c\"}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // All are inline, should be ignored
        assert!(result.is_none(), "All three inline should be ignored");
    }

    /// Test mixed: one inline (ignored), one standalone (detected)
    #[test]
    fn test_mixed_standalone_and_inline() {
        let input = "Text with {\"tool\": \"inline\"} here\n{\"tool\": \"standalone\", \"args\": {}}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Should find the standalone one, not the inline one
        assert!(result.is_some(), "Should find standalone");
        // The standalone one starts after the newline
        let newline_pos = input.find('\n').unwrap();
        assert_eq!(result.unwrap(), newline_pos + 1, "Should find standalone after newline");
    }
}

// =============================================================================
// Test: Edge cases that should NOT be detected (not tool patterns)
// =============================================================================

mod no_detection_cases {
    use super::*;

    /// Test similar but not matching patterns
    #[test]
    fn test_similar_but_different() {
        let inputs = [
            "{\"tools\": \"value\"}",  // "tools" not "tool"
            "{\"Tool\": \"value\"}",  // Capital T
            "{\"TOOL\": \"value\"}",  // All caps
            "{'tool': 'value'}",       // Single quotes
        ];
        
        for input in inputs {
            let result = StreamingToolParser::find_first_tool_call_start(input);
            assert!(result.is_none(), "'{}' should not be detected", input);
        }
    }

    /// Test partial patterns
    #[test]
    fn test_partial_patterns() {
        let inputs = [
            "{\"tool",           // No colon
            "\"tool\":",         // No opening brace
            "tool",              // Just the word
        ];
        
        for input in inputs {
            let result = StreamingToolParser::find_first_tool_call_start(input);
            assert!(result.is_none(), "'{}' should not be detected", input);
        }
    }

    /// Test JSON that happens to have "tool" as a value
    #[test]
    fn test_tool_as_value() {
        let input = "{\"name\": \"tool\"}";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        assert!(result.is_none(), "'tool' as value should not trigger detection");
    }
}

// =============================================================================
// Test: Real-world scenarios from the bug report
// =============================================================================

mod real_world_scenarios {
    use super::*;

    /// Test documentation example that caused the original bug
    #[test]
    fn test_documentation_example() {
        let input = r#"To call a tool, use this format: {"tool": "name", "args": {...}}

For example:
{"tool": "shell", "args": {"command": "ls"}}

This will execute the command."#;
        
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Should find the standalone example, not the inline one
        assert!(result.is_some(), "Should find standalone example");
        
        // The standalone example is on line 4 (0-indexed line 3)
        // "To call a tool...\n\nFor example:\n" = 64 + 1 + 13 + 1 = 79 chars before it
        // Actually let's just verify it's NOT at position 33 (the inline one)
        assert!(result.unwrap() > 50, "Should skip inline and find standalone");
    }

    /// Test code example in prose - should be IGNORED
    #[test]
    fn test_code_in_prose() {
        let input = "The agent responds with {\"tool\": \"read_file\"} when it needs to read files.";
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        assert!(result.is_none(), "Code in prose should be ignored");
    }

    /// Test the exact scenario from the bug: LLM explaining tool format
    #[test]
    fn test_llm_explanation_scenario() {
        let input = r#"I'll use the shell tool. The format is {"tool": "shell", "args": {...}}.

{"tool": "shell", "args": {"command": "ls -la"}}"#;
        
        let result = StreamingToolParser::find_first_tool_call_start(input);
        
        // Should find the actual tool call, not the explanation
        assert!(result.is_some(), "Should find actual tool call");
        
        // The actual tool call is on the last line, after two newlines
        let last_newline = input.rfind('\n').unwrap();
        assert_eq!(result.unwrap(), last_newline + 1, "Should find tool call on last line");
    }
}

// =============================================================================
// Test: is_on_own_line helper function
// =============================================================================

mod is_on_own_line_tests {
    use super::*;

    #[test]
    fn test_position_zero() {
        assert!(StreamingToolParser::is_on_own_line("anything", 0));
    }

    #[test]
    fn test_after_newline_no_whitespace() {
        let text = "line1\nline2";
        assert!(StreamingToolParser::is_on_own_line(text, 6)); // position of 'l' in line2
    }

    #[test]
    fn test_after_newline_with_whitespace() {
        let text = "line1\n  indented";
        assert!(StreamingToolParser::is_on_own_line(text, 8)); // position of 'i' in indented
    }

    #[test]
    fn test_middle_of_line() {
        let text = "some text here";
        assert!(!StreamingToolParser::is_on_own_line(text, 5)); // position of 't' in text
    }

    #[test]
    fn test_after_non_whitespace() {
        let text = "prefix{";
        assert!(!StreamingToolParser::is_on_own_line(text, 6)); // position of '{'
    }
}

// =============================================================================
// Test: End-to-end streaming repro of the parser poisoning bug
// =============================================================================

mod streaming_repro {
    use super::*;
    use g3_providers::CompletionChunk;

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

    /// EXACT REPRO: LLM explains tool format inline, then emits real tool call.
    /// Before the fix, the parser would detect the inline pattern and try to
    /// parse it as a tool call, causing premature return of control.
    #[test]
    fn test_inline_explanation_does_not_trigger_tool_detection() {
        let mut parser = StreamingToolParser::new();

        // Simulate streaming chunks as the LLM explains tool format
        let tools = parser.process_chunk(&chunk(
            "I'll help you with that. The tool call format is ",
            false,
        ));
        assert!(tools.is_empty(), "No tool call yet");

        // THIS IS THE BUG: inline JSON pattern in explanation
        let tools = parser.process_chunk(&chunk(
            r#"{"tool": "shell", "args": {...}}"#,
            false,
        ));
        // Before fix: this would incorrectly detect a tool call
        // After fix: this should be ignored (it's inline, not on its own line)
        assert!(tools.is_empty(), "Inline pattern should NOT trigger tool detection");

        // More explanation
        let tools = parser.process_chunk(&chunk(
            " where you specify the command.\n\n",
            false,
        ));
        assert!(tools.is_empty(), "Still no tool call");

        // NOW the real tool call on its own line
        let tools = parser.process_chunk(&chunk(
            r#"{"tool": "shell", "args": {"command": "ls -la"}}"#,
            true,
        ));
        
        // Should detect exactly ONE tool call - the real one
        assert_eq!(tools.len(), 1, "Should detect exactly one tool call");
        assert_eq!(tools[0].tool, "shell");
        assert_eq!(tools[0].args["command"], "ls -la");
    }

    /// Test that multiple inline patterns in a single chunk are all ignored
    #[test]
    fn test_multiple_inline_patterns_in_chunk_ignored() {
        let mut parser = StreamingToolParser::new();

        let tools = parser.process_chunk(&chunk(
            r#"Compare {"tool": "a"} with {"tool": "b"} and {"tool": "c"}"#,
            true,
        ));

        assert!(tools.is_empty(), "All inline patterns should be ignored");
    }

    /// Test streaming where tool call arrives across multiple chunks
    #[test]
    fn test_tool_call_split_across_chunks() {
        let mut parser = StreamingToolParser::new();

        // First chunk: prose then start of tool call on new line
        let tools = parser.process_chunk(&chunk("Here's the command:\n{\"tool\": ", false));
        assert!(tools.is_empty(), "Incomplete tool call");

        // Second chunk: rest of tool call
        let tools = parser.process_chunk(&chunk(
            r#""shell", "args": {"command": "pwd"}}"#,
            true,
        ));

        assert_eq!(tools.len(), 1, "Should detect the complete tool call");
        assert_eq!(tools[0].tool, "shell");
    }
}

/// Test that inline JSON is not detected even when stream finishes
/// This tests the try_parse_all_json_tool_calls_from_buffer path
#[test]
fn test_inline_json_not_detected_at_stream_end() {
    use g3_core::StreamingToolParser;
    use g3_providers::CompletionChunk;
    
    fn chunk(content: &str, finished: bool) -> CompletionChunk {
        CompletionChunk {
            content: content.to_string(),
            finished,
            tool_calls: None,
            usage: None,
            stop_reason: if finished { Some("end_turn".to_string()) } else { None },
            tool_call_streaming: None,
            upstream_ping: false,
        }
    }
    
    let mut parser = StreamingToolParser::new();
    
    // Send chunks exactly as MockProvider would
    let tools = parser.process_chunk(&chunk("To run a command, you can use the format ", false));
    assert!(tools.is_empty(), "Chunk 1: no tools");
    
    let tools = parser.process_chunk(&chunk(r#"{"tool": "shell", "args": {"command": "ls"}}"#, false));
    assert!(tools.is_empty(), "Chunk 2: inline JSON should not trigger tool detection");
    
    let tools = parser.process_chunk(&chunk(" in your request. ", false));
    assert!(tools.is_empty(), "Chunk 3: no tools");
    
    let tools = parser.process_chunk(&chunk("Let me know if you need help!", false));
    assert!(tools.is_empty(), "Chunk 4: no tools");
    
    // Finish chunk - this triggers try_parse_all_json_tool_calls_from_buffer
    let tools = parser.process_chunk(&chunk("", true));
    assert!(
        tools.is_empty(),
        "Finish chunk: inline JSON should NOT be detected as tool call. Found: {:?}",
        tools.iter().map(|t| &t.tool).collect::<Vec<_>>()
    );
    
    // Verify the full buffer content
    let buffer = parser.get_text_content();
    assert!(
        buffer.contains(r#"{"tool": "shell"#),
        "Buffer should contain the inline JSON: {}",
        buffer
    );
}
