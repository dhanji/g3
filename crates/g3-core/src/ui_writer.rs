/// Interface for UI output operations
/// This trait abstracts all UI operations to allow different implementations
/// (console, TUI, web, etc.) without coupling the core logic to specific output methods.
pub trait UiWriter: Send + Sync {
    /// Print a simple message
    fn print(&self, message: &str);

    /// Print a message with a newline
    fn println(&self, message: &str);

    /// Print without newline (for progress indicators)
    fn print_inline(&self, message: &str);

    /// Print a system prompt section
    fn print_system_prompt(&self, prompt: &str);

    /// Print a context window status message
    fn print_context_status(&self, message: &str);

    /// Print a g3-style status message in progress
    /// Format: "g3: <message> ..."
    /// - "g3:" should be bold green
    fn print_g3_progress(&self, message: &str);

    /// Print a g3-style status message with completion status
    /// Format: "g3: <message> ... [status]"
    /// - "g3:" should be bold green, "failed"/"error" status should be red
    fn print_g3_status(&self, message: &str, status: &str);

    /// Print a context thinning result
    fn print_thin_result(&self, result: &crate::ThinResult);

    /// Print a context thinning message (legacy - for pre-formatted messages)
    fn print_context_thinning(&self, _message: &str) {}

    /// Print end-of-turn context window summary.
    /// Default: no-op. The CLI's SimpleUiWriter renders the dot-bar; the
    /// EventStreamWriter tees an NDJSON `context_summary` event.
    fn print_context_summary(&self, _used_tokens: u32, _total_tokens: u32, _percentage: f32) {}

    /// Print a tool execution header
    fn print_tool_header(&self, tool_name: &str, tool_args: Option<&serde_json::Value>);

    /// Print a tool argument
    fn print_tool_arg(&self, key: &str, value: &str);

    /// Print tool output header
    fn print_tool_output_header(&self);

    /// Update the current tool output line (replaces previous line)
    fn update_tool_output_line(&self, line: &str);

    /// Print a tool output line
    fn print_tool_output_line(&self, line: &str);

    /// Print tool output summary (when output is truncated)
    fn print_tool_output_summary(&self, hidden_count: usize);

    /// Print a compact single-line tool output (for file operations)
    /// Format: " ● tool_name | path [range] | summary | tokens ◉ time"
    /// Returns true if the tool was handled in compact format, false to use normal format
    fn print_tool_compact(&self, _tool_name: &str, _summary: &str, _duration_str: &str, _tokens_delta: u32, _context_percentage: f32) -> bool {
        // Default: don't use compact format
        false
    }

    /// Print a compact TODO tool output with styled content
    /// Format: " ● todo_read" header, then styled content lines, no timing
    /// content: None for empty, Some(content) for TODO content
    /// is_write: true for todo_write, false for todo_read
    /// Returns true if handled, false to fall back to normal format
    fn print_todo_compact(&self, _content: Option<&str>, _is_write: bool) -> bool {
        false
    }

    /// Print a compact Plan tool output with styled content
    /// Format: " ● plan_read" or " ● plan_write" header, then styled plan items
    /// plan_yaml: None for no plan, Some(yaml) for plan content
    /// plan_file_path: Path to the plan file for user reference
    /// is_write: true for plan_write, false for plan_read
    /// Returns true if handled, false to fall back to normal format
    fn print_plan_compact(&self, _plan_yaml: Option<&str>, _plan_file_path: Option<&str>, _is_write: bool) -> bool {
        false
    }

    /// Print a compact write_envelope output showing pipeline stages.
    /// Displays: envelope written → rulespec compiled → verification → token stamped
    /// fact_groups: number of top-level fact groups in the envelope
    /// stages: list of (flat_icon, description) pairs for each completed stage
    /// passed: number of predicates that passed (None if no rulespec)
    /// total: total number of predicates (None if no rulespec)
    /// failed: number of predicates that failed
    fn print_envelope_compact(&self, _fact_groups: usize, _stages: &[(& str, &str)], _passed: Option<usize>, _total: Option<usize>, _failed: usize) {
        // Default: no-op (NullUiWriter inherits this)
    }

    /// Print tool execution timing
    fn print_tool_timing(&self, duration_str: &str, tokens_delta: u32, context_percentage: f32);

    /// Print the agent prompt indicator
    fn print_agent_prompt(&self);

    /// Print agent response inline (for streaming)
    fn print_agent_response(&self, content: &str);

    /// Notify that an SSE event was received (including pings)
    fn notify_sse_received(&self);

    /// Print a hint that a tool call is being streamed (show indicator immediately)
    /// This is called when the provider starts receiving a tool call but args are still streaming
    fn print_tool_streaming_hint(&self, tool_name: &str);

    /// Signal that a tool call is still actively streaming (for blinking indicator)
    /// This is called periodically while tool args are being received
    fn print_tool_streaming_active(&self);

    /// Flush any buffered output
    fn flush(&self);

    /// Returns true if this UI writer wants full, untruncated output
    /// Default is false (truncate for human readability)
    fn wants_full_output(&self) -> bool {
        false
    }

    /// Prompt the user for a yes/no confirmation
    fn prompt_user_yes_no(&self, message: &str) -> bool;

    /// Prompt the user to choose from a list of options
    /// Returns the index of the selected option
    fn prompt_user_choice(&self, message: &str, options: &[&str]) -> usize;

    /// Filter JSON tool calls from streaming content for display.
    /// This is a UI concern - the raw content should be preserved for logging.
    /// Default implementation passes through unchanged.
    fn filter_json_tool_calls(&self, content: &str) -> String {
        content.to_string()
    }

    /// Reset the JSON tool call filter state.
    /// Called at the start of a new response to clear any partial state.
    /// Default implementation does nothing.
    fn reset_json_filter(&self) {}
    
    /// Finish the streaming markdown formatter and flush any remaining content.
    /// Called at the end of an agent response to emit any buffered markdown.
    /// Also resets the formatter for the next response.
    /// Default implementation does nothing.
    fn finish_streaming_markdown(&self) {}

    /// Set whether the UI is in agent mode.
    /// When in agent mode, tool names may be displayed differently (e.g., different color).
    /// Default implementation does nothing.
    fn set_agent_mode(&self, _is_agent_mode: bool) {}

    /// Set the workspace path for shortening displayed paths.
    /// Paths under this directory will be shown as `./relative/path`.
    /// Default implementation does nothing.
    fn set_workspace_path(&self, _path: std::path::PathBuf) {}

    /// Set the active project path and name for shortening displayed paths.
    /// Paths under this directory will be shown as `<project_name>/relative/path`.
    /// Default implementation does nothing.
    fn set_project_path(&self, _path: std::path::PathBuf, _name: String) {}

    /// Clear the active project (when project is unloaded).
    /// Default implementation does nothing.
    fn clear_project(&self) {}
}

/// A no-op implementation for when UI output is not needed
pub struct NullUiWriter;

impl UiWriter for NullUiWriter {
    fn print(&self, _message: &str) {}
    fn println(&self, _message: &str) {}
    fn print_inline(&self, _message: &str) {}
    fn print_system_prompt(&self, _prompt: &str) {}
    fn print_context_status(&self, _message: &str) {}
    fn print_g3_progress(&self, _message: &str) {}
    fn print_g3_status(&self, _message: &str, _status: &str) {}
    fn print_thin_result(&self, _result: &crate::ThinResult) {}
    fn print_tool_header(&self, _tool_name: &str, _tool_args: Option<&serde_json::Value>) {}
    fn print_tool_arg(&self, _key: &str, _value: &str) {}
    fn print_tool_output_header(&self) {}
    fn update_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_summary(&self, _hidden_count: usize) {}
    fn print_tool_timing(&self, _duration_str: &str, _tokens_delta: u32, _context_percentage: f32) {}
    fn print_agent_prompt(&self) {}
    fn print_agent_response(&self, _content: &str) {}
    fn notify_sse_received(&self) {}
    fn print_tool_streaming_hint(&self, _tool_name: &str) {}
    fn print_tool_streaming_active(&self) {}
    fn flush(&self) {}
    fn finish_streaming_markdown(&self) {}
    fn wants_full_output(&self) -> bool {
        false
    }
    fn prompt_user_yes_no(&self, _message: &str) -> bool {
        true
    }
    fn prompt_user_choice(&self, _message: &str, _options: &[&str]) -> usize {
        0
    }
}
