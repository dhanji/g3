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
    
    /// Print a tool execution header
    fn print_tool_header(&self, tool_name: &str);
    
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
    
    /// Print tool execution timing
    fn print_tool_timing(&self, duration_str: &str);
    
    /// Print the agent prompt indicator
    fn print_agent_prompt(&self);
    
    /// Print agent response inline (for streaming)
    fn print_agent_response(&self, content: &str);
    
    /// Notify that an SSE event was received (including pings)
    fn notify_sse_received(&self);
    
    /// Flush any buffered output
    fn flush(&self);
}

/// A no-op implementation for when UI output is not needed
pub struct NullUiWriter;

impl UiWriter for NullUiWriter {
    fn print(&self, _message: &str) {}
    fn println(&self, _message: &str) {}
    fn print_inline(&self, _message: &str) {}
    fn print_system_prompt(&self, _prompt: &str) {}
    fn print_context_status(&self, _message: &str) {}
    fn print_tool_header(&self, _tool_name: &str) {}
    fn print_tool_arg(&self, _key: &str, _value: &str) {}
    fn print_tool_output_header(&self) {}
    fn update_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_line(&self, _line: &str) {}
    fn print_tool_output_summary(&self, _hidden_count: usize) {}
    fn print_tool_timing(&self, _duration_str: &str) {}
    fn print_agent_prompt(&self) {}
    fn print_agent_response(&self, _content: &str) {}
    fn notify_sse_received(&self) {}
    fn flush(&self) {}
}