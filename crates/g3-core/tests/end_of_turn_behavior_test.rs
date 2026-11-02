//! End-of-Turn Behavior Integration Tests
//!
//! CHARACTERIZATION: These tests verify the observable behavior of end-of-turn
//! logic through stable public interfaces.
//!
//! What these tests protect:
//! - Timing footer formatting (observable output)
//! - Tool call duplicate detection (prevents stuttering)
//! - Empty response detection (triggers auto-continue)
//! - Connection error classification (enables graceful recovery)
//! - Tool output summary formatting (user-facing display)
//!
//! What these tests intentionally do NOT assert:
//! - Internal streaming state machine transitions
//! - Specific iteration counts or loop behavior
//! - Internal parser buffer management
//! - Provider-specific response handling

use g3_core::streaming::{
    are_tool_calls_duplicate, format_timing_footer, is_connection_error, is_empty_response,
    format_read_file_summary, format_write_file_result, format_str_replace_summary,
    format_remember_summary, format_screenshot_summary, format_coverage_summary,
    format_duration, truncate_for_display, truncate_line,
    clean_llm_tokens,
};
use g3_core::ToolCall;
use std::time::Duration;

// =============================================================================
// Test: Timing Footer Formatting
// =============================================================================

mod timing_footer {
    use super::*;

    /// Test basic timing footer with all components
    #[test]
    fn test_format_timing_footer_complete() {
        let elapsed = Duration::from_secs(5);
        let ttft = Duration::from_millis(500);
        let turn_tokens = Some(1500);
        let context_pct = 45.5;

        let footer = format_timing_footer(elapsed, ttft, turn_tokens, context_pct);

        // Should contain timing info
        assert!(footer.contains("5.0s"), "Should show elapsed time: {}", footer);
        assert!(footer.contains("500ms"), "Should show TTFT: {}", footer);
        // Should contain token info
        assert!(footer.contains("1500") || footer.contains("1.5k"), "Should show tokens: {}", footer);
        // Should contain context percentage
        assert!(footer.contains("45") || footer.contains("46"), "Should show context %: {}", footer);
    }

    /// Test timing footer without token info
    #[test]
    fn test_format_timing_footer_no_tokens() {
        let elapsed = Duration::from_secs(10);
        let ttft = Duration::from_secs(1);
        let turn_tokens = None;
        let context_pct = 30.0;

        let footer = format_timing_footer(elapsed, ttft, turn_tokens, context_pct);

        // Should still have timing
        assert!(footer.contains("10"), "Should show elapsed time: {}", footer);
        // Should handle missing tokens gracefully
        assert!(!footer.is_empty(), "Footer should not be empty");
    }

    /// Test timing footer with very short times (milliseconds)
    #[test]
    fn test_format_timing_footer_short_times() {
        let elapsed = Duration::from_millis(250);
        let ttft = Duration::from_millis(50);
        let turn_tokens = Some(100);
        let context_pct = 5.0;

        let footer = format_timing_footer(elapsed, ttft, turn_tokens, context_pct);

        // Should format milliseconds appropriately
        assert!(footer.contains("ms") || footer.contains("0."), "Should handle ms times: {}", footer);
    }

    /// Test timing footer with long times (minutes)
    #[test]
    fn test_format_timing_footer_long_times() {
        let elapsed = Duration::from_secs(125); // 2m 5s
        let ttft = Duration::from_secs(3);
        let turn_tokens = Some(50000);
        let context_pct = 85.0;

        let footer = format_timing_footer(elapsed, ttft, turn_tokens, context_pct);

        // Should format minutes appropriately
        assert!(footer.contains("m") || footer.contains("125"), "Should handle minute times: {}", footer);
    }

    /// Test timing footer at context capacity
    #[test]
    fn test_format_timing_footer_high_context() {
        let elapsed = Duration::from_secs(30);
        let ttft = Duration::from_secs(2);
        let turn_tokens = Some(180000);
        let context_pct = 95.0;

        let footer = format_timing_footer(elapsed, ttft, turn_tokens, context_pct);

        // Should show high context percentage
        assert!(footer.contains("95") || footer.contains("9"), "Should show high context: {}", footer);
    }
}

// =============================================================================
// Test: Tool Call Duplicate Detection
// =============================================================================

mod duplicate_detection {
    use super::*;

    fn make_tool_call_with_id(tool: &str, args: serde_json::Value, id: &str) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            args,
            id: id.to_string(),
        }
    }
    fn make_tool_call(tool: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            args,
            id: String::new(),
        }
    }

    /// Test identical tool calls are detected as duplicates
    #[test]
    fn test_identical_calls_are_duplicates() {
        let tc1 = make_tool_call("read_file", serde_json::json!({"file_path": "test.txt"}));
        let tc2 = make_tool_call("read_file", serde_json::json!({"file_path": "test.txt"}));

        assert!(are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test different tools are not duplicates
    #[test]
    fn test_different_tools_not_duplicates() {
        let tc1 = make_tool_call("read_file", serde_json::json!({"file_path": "test.txt"}));
        let tc2 = make_tool_call("write_file", serde_json::json!({"file_path": "test.txt"}));

        assert!(!are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test same tool with different args are not duplicates
    #[test]
    fn test_same_tool_different_args_not_duplicates() {
        let tc1 = make_tool_call("read_file", serde_json::json!({"file_path": "a.txt"}));
        let tc2 = make_tool_call("read_file", serde_json::json!({"file_path": "b.txt"}));

        assert!(!are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test empty args are handled correctly
    #[test]
    fn test_empty_args_duplicates() {
        let tc1 = make_tool_call("todo_read", serde_json::json!({}));
        let tc2 = make_tool_call("todo_read", serde_json::json!({}));

        assert!(are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test complex nested args
    #[test]
    fn test_complex_args_duplicates() {
        let args = serde_json::json!({
            "searches": [
                {"name": "test", "query": "(function_item)", "language": "rust"}
            ]
        });
        let tc1 = make_tool_call("code_search", args.clone());
        let tc2 = make_tool_call("code_search", args);

        assert!(are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test complex args with different values
    #[test]
    fn test_complex_args_different_not_duplicates() {
        let args1 = serde_json::json!({
            "searches": [{"name": "test1", "query": "(function_item)"}]
        });
        let args2 = serde_json::json!({
            "searches": [{"name": "test2", "query": "(function_item)"}]
        });
        let tc1 = make_tool_call("code_search", args1);
        let tc2 = make_tool_call("code_search", args2);

        assert!(!are_tool_calls_duplicate(&tc1, &tc2));
    }

    // =========================================================================
    // ID-ignorant deduplication tests (are_tool_calls_duplicate is name+args only)
    // ID-aware cross-message dedup lives in check_duplicate_in_previous_message()
    // and is tested via integration tests in mock_provider_integration_test.rs
    // =========================================================================

    /// Test: are_tool_calls_duplicate ignores IDs — same name+args = duplicate
    /// regardless of ID. The ID-aware logic is in check_duplicate_in_previous_message.
    #[test]
    fn test_different_ids_same_args_still_duplicate_at_chunk_level() {
        let tc1 = make_tool_call_with_id(
            "research_status",
            serde_json::json!({}),
            "toolu_01ABC",
        );
        let tc2 = make_tool_call_with_id(
            "research_status",
            serde_json::json!({}),
            "toolu_01DEF",
        );

        // Same name+args = duplicate (IDs are ignored at this level)
        assert!(are_tool_calls_duplicate(&tc1, &tc2));
    }

    /// Test: Different IDs with different args are NOT duplicates.
    #[test]
    fn test_different_ids_different_args_not_duplicate() {
        let tc1 = make_tool_call_with_id(
            "research_status",
            serde_json::json!({"research_id": "r_123"}),
            "toolu_01ABC",
        );
        let tc2 = make_tool_call_with_id(
            "research_status",
            serde_json::json!({"research_id": "r_456"}),
            "toolu_01DEF",
        );

        assert!(!are_tool_calls_duplicate(&tc1, &tc2));
    }
}

// =============================================================================
// Test: Empty Response Detection
// =============================================================================

mod empty_response {
    use super::*;

    /// Test truly empty responses
    #[test]
    fn test_empty_string() {
        assert!(is_empty_response(""));
    }

    /// Test whitespace-only responses
    #[test]
    fn test_whitespace_only() {
        assert!(is_empty_response("   "));
        assert!(is_empty_response("\n\n\n"));
        assert!(is_empty_response("  \n  \t  \n  "));
    }

    /// Test timing-only responses (should be considered empty)
    #[test]
    fn test_timing_only() {
        assert!(is_empty_response("⏱️ 43.0s | 💭 3.6s"));
        assert!(is_empty_response("  ⏱️ 43.0s | 💭 3.6s  "));
        assert!(is_empty_response("\n⏱️ 43.0s | 💭 3.6s\n"));
    }

    /// Test mixed timing and whitespace
    #[test]
    fn test_timing_with_whitespace() {
        assert!(is_empty_response("\n\n⏱️ 10.0s | 💭 1.0s\n\n"));
        assert!(is_empty_response("⏱️ 1s\n\n⏱️ 2s"));
    }

    /// Test substantive content is NOT empty
    #[test]
    fn test_substantive_content_not_empty() {
        assert!(!is_empty_response("Hello"));
        assert!(!is_empty_response("I will help you."));
        assert!(!is_empty_response("Done!"));
        assert!(!is_empty_response("."));
    }

    /// Test timing with substantive content is NOT empty
    #[test]
    fn test_timing_with_content_not_empty() {
        assert!(!is_empty_response("⏱️ 43.0s\nHere is the result."));
        assert!(!is_empty_response("Done!\n⏱️ 43.0s"));
    }

    /// Test JSON tool calls are NOT empty
    #[test]
    fn test_json_not_empty() {
        assert!(!is_empty_response(r#"{"tool": "read_file"}"#));
        assert!(!is_empty_response(r#"{"tool": "test", "args": {}}"#));
    }

    /// Test code blocks are NOT empty
    #[test]
    fn test_code_blocks_not_empty() {
        assert!(!is_empty_response("```rust\nfn main() {}\n```"));
    }

    /// Test markdown is NOT empty
    #[test]
    fn test_markdown_not_empty() {
        assert!(!is_empty_response("# Summary"));
        assert!(!is_empty_response("- Item 1"));
    }
}

// =============================================================================
// Test: Connection Error Detection
// =============================================================================

mod connection_errors {
    use super::*;

    /// Test EOF errors are detected
    #[test]
    fn test_eof_errors() {
        assert!(is_connection_error("unexpected EOF during read"));
        assert!(is_connection_error("unexpected EOF"));
    }

    /// Test connection errors are detected
    #[test]
    fn test_connection_errors() {
        assert!(is_connection_error("connection reset"));
        assert!(is_connection_error("connection refused"));
        assert!(is_connection_error("connection timed out"));
    }

    /// Test chunk errors are detected
    #[test]
    fn test_chunk_errors() {
        assert!(is_connection_error("chunk size line"));
        assert!(is_connection_error("invalid chunk size line"));
    }

    /// Test body errors are detected
    #[test]
    fn test_body_errors() {
        assert!(is_connection_error("body error"));
        assert!(is_connection_error("body error occurred"));
    }

    /// Test non-connection errors are NOT detected
    #[test]
    fn test_non_connection_errors() {
        assert!(!is_connection_error("invalid JSON"));
        assert!(!is_connection_error("rate limit exceeded"));
        assert!(!is_connection_error("authentication failed"));
        assert!(!is_connection_error("model not found"));
    }
}

// =============================================================================
// Test: Tool Output Summary Formatting
// =============================================================================

mod tool_output_formatting {
    use super::*;

    /// Test read_file summary formatting
    #[test]
    fn test_read_file_summary() {
        assert_eq!(format_read_file_summary(10, 500), "10 lines (500 chars)");
        assert_eq!(format_read_file_summary(100, 1500), "100 lines (1.5k chars)");
        assert_eq!(format_read_file_summary(1, 50), "1 lines (50 chars)");
        assert_eq!(format_read_file_summary(0, 0), "0 lines (0 chars)");
    }

    /// Test read_file summary with large files
    #[test]
    fn test_read_file_summary_large() {
        let summary = format_read_file_summary(5000, 250000);
        assert!(summary.contains("5000"));
        assert!(summary.contains("250.0k") || summary.contains("250k"));
    }

    /// Test write_file result parsing
    #[test]
    fn test_write_file_result() {
        let result = format_write_file_result("wrote 42 lines | 1500 chars");
        assert!(result.contains("42"), "Should contain line count: {}", result);
        assert!(result.contains("1500"), "Should contain char count: {}", result);
    }

    /// Test write_file result with k notation
    #[test]
    fn test_write_file_result_k_notation() {
        let result = format_write_file_result("wrote 100 lines | 2.5k chars");
        assert!(result.contains("100"));
        assert!(result.contains("2.5k"));
    }

    /// Test write_file result fallback for unexpected format
    #[test]
    fn test_write_file_result_fallback() {
        let result = format_write_file_result("unexpected format");
        assert_eq!(result, "unexpected format");
    }

    /// Test str_replace summary with both insertions and deletions
    #[test]
    fn test_str_replace_summary_both() {
        let summary = format_str_replace_summary(5, 3);
        assert!(summary.contains("+5") || summary.contains("5"));
        assert!(summary.contains("-3") || summary.contains("3"));
    }

    /// Test str_replace summary with only insertions
    #[test]
    fn test_str_replace_summary_insertions_only() {
        let summary = format_str_replace_summary(10, 0);
        assert!(summary.contains("10"));
    }

    /// Test str_replace summary with only deletions
    #[test]
    fn test_str_replace_summary_deletions_only() {
        let summary = format_str_replace_summary(0, 7);
        assert!(summary.contains("7"));
    }

    /// Test remember summary parsing
    #[test]
    fn test_remember_summary() {
        let summary = format_remember_summary("Memory updated. Size: 1.2k");
        assert!(summary.contains("1.2k") || summary.contains("memory"));
    }

    /// Test remember summary fallback
    #[test]
    fn test_remember_summary_fallback() {
        let summary = format_remember_summary("Memory updated");
        assert!(summary.contains("memory"));
    }

    /// Test screenshot summary parsing
    #[test]
    fn test_screenshot_summary() {
        let summary = format_screenshot_summary("✅ Screenshot of Safari saved to: /tmp/screenshot.png");
        assert!(summary.contains("screenshot.png") || summary.contains("📸"));
    }

    /// Test screenshot summary error case
    #[test]
    fn test_screenshot_summary_error() {
        let summary = format_screenshot_summary("❌ Failed to capture screenshot");
        assert!(summary.contains("❌") || summary.contains("failed"));
    }

    /// Test coverage summary
    #[test]
    fn test_coverage_summary() {
        let summary = format_coverage_summary("Coverage report generated");
        assert!(summary.contains("📊") || summary.contains("report"));
    }

    /// Test coverage summary error case
    #[test]
    fn test_coverage_summary_error() {
        let summary = format_coverage_summary("❌ Coverage failed");
        assert!(summary.contains("❌") || summary.contains("failed"));
    }

}

// =============================================================================
// Test: Duration Formatting
// =============================================================================

mod duration_formatting {
    use super::*;

    /// Test millisecond formatting
    #[test]
    fn test_milliseconds() {
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        assert_eq!(format_duration(Duration::from_millis(50)), "50ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    /// Test second formatting
    #[test]
    fn test_seconds() {
        assert_eq!(format_duration(Duration::from_millis(1000)), "1.0s");
        assert_eq!(format_duration(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_duration(Duration::from_secs(30)), "30.0s");
    }

    /// Test minute formatting
    #[test]
    fn test_minutes() {
        let result = format_duration(Duration::from_secs(90));
        assert!(result.contains("m"), "Should format as minutes: {}", result);
        assert!(result.contains("1m") || result.contains("30"), "Should show 1m 30s: {}", result);
    }

    /// Test edge case: zero duration
    #[test]
    fn test_zero_duration() {
        let result = format_duration(Duration::from_millis(0));
        assert!(result.contains("0"), "Should handle zero: {}", result);
    }
}

// =============================================================================
// Test: Text Truncation
// =============================================================================

mod truncation {
    use super::*;

    /// Test truncate_for_display with short strings
    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate_for_display("short", 10), "short");
        assert_eq!(truncate_for_display("exact", 5), "exact");
    }

    /// Test truncate_for_display with long strings
    #[test]
    fn test_truncate_long_string() {
        let result = truncate_for_display("this is a very long string", 10);
        assert!(result.len() <= 15, "Should be truncated: {}", result);
        assert!(result.ends_with("..."), "Should end with ellipsis: {}", result);
    }

    /// Test truncate_for_display with multiline (uses first line only)
    #[test]
    fn test_truncate_multiline() {
        assert_eq!(truncate_for_display("first line\nsecond line", 20), "first line");
        assert_eq!(truncate_for_display("❌ Error\nDetails here", 10), "❌ Error");
    }

    /// Test truncate_line with should_truncate flag
    #[test]
    fn test_truncate_line_flag() {
        let long_line = "a".repeat(100);
        
        // With truncation enabled
        let truncated = truncate_line(&long_line, 50, true);
        assert!(truncated.len() <= 55, "Should be truncated: len={}", truncated.len());
        
        // With truncation disabled
        let not_truncated = truncate_line(&long_line, 50, false);
        assert_eq!(not_truncated.len(), 100, "Should not be truncated");
    }
}

// =============================================================================
// Test: LLM Token Cleaning
// =============================================================================

mod token_cleaning {
    use super::*;

    /// Test removal of common LLM stop tokens
    #[test]
    fn test_clean_stop_tokens() {
        assert_eq!(clean_llm_tokens("hello<|im_end|>"), "hello");
        assert_eq!(clean_llm_tokens("test</s>more"), "testmore");
        assert_eq!(clean_llm_tokens("[/INST]response"), "response");
    }

    /// Test content without tokens is unchanged
    #[test]
    fn test_clean_no_tokens() {
        assert_eq!(clean_llm_tokens("normal text"), "normal text");
        assert_eq!(clean_llm_tokens(""), "");
    }

    /// Test multiple tokens in one string
    #[test]
    fn test_clean_multiple_tokens() {
        let result = clean_llm_tokens("start<|im_end|>middle</s>end");
        assert!(!result.contains("<|im_end|>"));
        assert!(!result.contains("</s>"));
    }
}

// =============================================================================
// Test: Edge Cases and Boundary Conditions
// =============================================================================

mod edge_cases {
    use super::*;

    /// Test empty inputs don't panic
    #[test]
    fn test_empty_inputs() {
        assert_eq!(clean_llm_tokens(""), "");
        assert_eq!(truncate_for_display("", 10), "");
        assert_eq!(truncate_line("", 10, true), "");
        assert!(is_empty_response(""));
        assert!(!is_connection_error(""));
    }

    /// Test unicode handling in truncation
    #[test]
    fn test_unicode_truncation() {
        // Emoji and special characters should be handled safely
        let emoji_str = "🎉🎊🎈🎁🎀";
        let result = truncate_for_display(emoji_str, 3);
        // Should not panic and should produce valid UTF-8
        assert!(result.len() > 0);
        
        // Bullet points
        let bullet_str = "• Item 1\n• Item 2";
        let result = truncate_for_display(bullet_str, 10);
        assert!(result.starts_with("• Item"));
    }

    /// Test very large numbers in formatting
    #[test]
    fn test_large_numbers() {
        let summary = format_read_file_summary(1000000, 50000000);
        assert!(summary.contains("1000000") || summary.contains("M"));
    }

    /// Test timing footer with edge case values
    #[test]
    fn test_timing_footer_edge_values() {
        // Zero duration
        let footer = format_timing_footer(
            Duration::from_millis(0),
            Duration::from_millis(0),
            Some(0),
            0.0,
        );
        assert!(!footer.is_empty());

        // Very high context percentage
        let footer = format_timing_footer(
            Duration::from_secs(1),
            Duration::from_millis(100),
            Some(200000),
            100.0,
        );
        assert!(!footer.is_empty());
    }
}
