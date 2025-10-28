use g3_core::ui_writer::UiWriter;
use std::io::{self, Write};

/// Machine-mode implementation of UiWriter that prints plain, unformatted output
/// This is designed for programmatic consumption and outputs everything verbatim
pub struct MachineUiWriter;

impl MachineUiWriter {
    pub fn new() -> Self {
        Self
    }
}

impl UiWriter for MachineUiWriter {
    fn print(&self, message: &str) {
        print!("{}", message);
    }

    fn println(&self, message: &str) {
        println!("{}", message);
    }

    fn print_inline(&self, message: &str) {
        print!("{}", message);
        let _ = io::stdout().flush();
    }

    fn print_system_prompt(&self, prompt: &str) {
        println!("SYSTEM_PROMPT:");
        println!("{}", prompt);
        println!("END_SYSTEM_PROMPT");
        println!();
    }

    fn print_context_status(&self, message: &str) {
        println!("CONTEXT_STATUS: {}", message);
    }

    fn print_context_thinning(&self, message: &str) {
        println!("CONTEXT_THINNING: {}", message);
    }

    fn print_tool_header(&self, tool_name: &str) {
        println!("TOOL_CALL: {}", tool_name);
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        println!("TOOL_ARG: {} = {}", key, value);
    }

    fn print_tool_output_header(&self) {
        println!("TOOL_OUTPUT:");
    }

    fn update_tool_output_line(&self, line: &str) {
        println!("{}", line);
    }

    fn print_tool_output_line(&self, line: &str) {
        println!("{}", line);
    }

    fn print_tool_output_summary(&self, count: usize) {
        println!("TOOL_OUTPUT_LINES: {}", count);
    }

    fn print_tool_timing(&self, duration_str: &str) {
        println!("TOOL_DURATION: {}", duration_str);
        println!("END_TOOL_OUTPUT");
        println!();
    }

    fn print_agent_prompt(&self) {
        println!("AGENT_RESPONSE:");
        let _ = io::stdout().flush();
    }

    fn print_agent_response(&self, content: &str) {
        print!("{}", content);
        let _ = io::stdout().flush();
    }

    fn notify_sse_received(&self) {
        // No-op for machine mode
    }

    fn flush(&self) {
        let _ = io::stdout().flush();
    }
    
    fn wants_full_output(&self) -> bool {
        true  // Machine mode wants complete, untruncated output
    }
}
