use crate::retro_tui::RetroTui;
use g3_core::ui_writer::UiWriter;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::Instant;

/// Console implementation of UiWriter that prints to stdout
pub struct ConsoleUiWriter {
    current_tool_name: Mutex<Option<String>>,
    current_tool_args: Mutex<Vec<(String, String)>>,
}

impl ConsoleUiWriter {
    pub fn new() -> Self {
        Self {
            current_tool_name: Mutex::new(None),
            current_tool_args: Mutex::new(Vec::new()),
        }
    }
}

impl UiWriter for ConsoleUiWriter {
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
        println!("🔍 System Prompt:");
        println!("================");
        println!("{}", prompt);
        println!("================");
        println!();
    }

    fn print_context_status(&self, message: &str) {
        println!("{}", message);
    }

    fn print_tool_header(&self, tool_name: &str) {
        // Store the tool name and clear args for collection
        *self.current_tool_name.lock().unwrap() = Some(tool_name.to_string());
        self.current_tool_args.lock().unwrap().clear();
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        // Collect arguments instead of printing immediately
        self.current_tool_args
            .lock()
            .unwrap()
            .push((key.to_string(), value.to_string()));
    }

    fn print_tool_output_header(&self) {
        // Now print the tool header with the most important arg
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            let args = self.current_tool_args.lock().unwrap();

            // Find the most important argument
            let important_arg = args
                .iter()
                .find(|(k, _)| k == "command" || k == "file_path" || k == "path" || k == "diff")
                .or_else(|| args.first());

            if let Some((_, value)) = important_arg {
                // Truncate long values for display
                let display_value = if value.len() > 80 {
                    format!("{}...", &value[..77])
                } else {
                    value.clone()
                };
                println!("┌─ {} | {}", tool_name, display_value);
            } else {
                println!("┌─ {}", tool_name);
            }

            // Print any additional arguments (optional - can be removed if not wanted)
            for (key, value) in args.iter() {
                if Some(key.as_str()) != important_arg.map(|(k, _)| k.as_str()) {
                    // Only show additional args if they're short or important
                    if value.len() < 50 {
                        println!("  {}: {}", key, value);
                    }
                }
            }
        }
        //        println!("┌─────");
    }

    fn print_tool_output_line(&self, line: &str) {
        println!("│ {}", line);
    }

    fn print_tool_output_summary(&self, hidden_count: usize) {
        println!(
            "│ ... ({} more line{})",
            hidden_count,
            if hidden_count == 1 { "" } else { "s" }
        );
    }

    fn print_tool_timing(&self, duration_str: &str) {
        println!("└─ ⚡️ {}", duration_str);
        println!();
        // Clear the stored tool info
        *self.current_tool_name.lock().unwrap() = None;
        self.current_tool_args.lock().unwrap().clear();
    }

    fn print_agent_prompt(&self) {
        print!(" ");
        let _ = io::stdout().flush();
    }

    fn print_agent_response(&self, content: &str) {
        print!("{}", content);
        let _ = io::stdout().flush();
    }

    fn notify_sse_received(&self) {
        // No-op for console - we don't track SSEs in console mode
    }

    fn flush(&self) {
        let _ = io::stdout().flush();
    }
}

/// RetroTui implementation of UiWriter that sends output to the TUI
pub struct RetroTuiWriter {
    tui: RetroTui,
    current_tool_name: Mutex<Option<String>>,
    current_tool_output: Mutex<Vec<String>>,
    current_tool_start: Mutex<Option<Instant>>,
    current_tool_caption: Mutex<String>,
}

impl RetroTuiWriter {
    pub fn new(tui: RetroTui) -> Self {
        Self {
            tui,
            current_tool_name: Mutex::new(None),
            current_tool_output: Mutex::new(Vec::new()),
            current_tool_start: Mutex::new(None),
            current_tool_caption: Mutex::new(String::new()),
        }
    }
}

impl UiWriter for RetroTuiWriter {
    fn print(&self, message: &str) {
        self.tui.output(message);
    }

    fn println(&self, message: &str) {
        self.tui.output(message);
    }

    fn print_inline(&self, message: &str) {
        // For inline printing, we'll just append to the output
        self.tui.output(message);
    }

    fn print_system_prompt(&self, prompt: &str) {
        self.tui.output("🔍 System Prompt:");
        self.tui.output("================");
        for line in prompt.lines() {
            self.tui.output(line);
        }
        self.tui.output("================");
        self.tui.output("");
    }

    fn print_context_status(&self, message: &str) {
        self.tui.output(message);
    }

    fn print_tool_header(&self, tool_name: &str) {
        // Start collecting tool output
        *self.current_tool_start.lock().unwrap() = Some(Instant::now());
        *self.current_tool_name.lock().unwrap() = Some(tool_name.to_string());
        self.current_tool_output.lock().unwrap().clear();
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("Tool: {}", tool_name));

        // Initialize caption
        *self.current_tool_caption.lock().unwrap() = String::new();
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("{}: {}", key, value));

        // Build caption from first argument (usually the most important one)
        let mut caption = self.current_tool_caption.lock().unwrap();
        if caption.is_empty() && (key == "file_path" || key == "command" || key == "path") {
            // Truncate long values for the caption
            let truncated = if value.len() > 50 {
                format!("{}...", &value[..47])
            } else {
                value.to_string()
            };
            *caption = truncated;
        }
    }

    fn print_tool_output_header(&self) {
        // This is called right before tool execution starts
        // Send the initial tool header to the TUI now
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            let caption = self.current_tool_caption.lock().unwrap().clone();

            // Send the tool output with initial header
            self.tui.tool_output(tool_name, &caption, "");
        }

        self.current_tool_output.lock().unwrap().push(String::new());
        self.current_tool_output
            .lock()
            .unwrap()
            .push("Output:".to_string());
    }

    fn print_tool_output_line(&self, line: &str) {
        self.current_tool_output
            .lock()
            .unwrap()
            .push(line.to_string());
    }

    fn print_tool_output_summary(&self, hidden_count: usize) {
        self.current_tool_output.lock().unwrap().push(format!(
            "... ({} more line{})",
            hidden_count,
            if hidden_count == 1 { "" } else { "s" }
        ));
    }

    fn print_tool_timing(&self, duration_str: &str) {
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("⚡️ {}", duration_str));

        // Calculate the actual duration
        let duration_ms = if let Some(start) = *self.current_tool_start.lock().unwrap() {
            start.elapsed().as_millis()
        } else {
            0
        };

        // Get the tool name and caption
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            let content = self.current_tool_output.lock().unwrap().join("\n");
            let caption = self.current_tool_caption.lock().unwrap().clone();
            let caption = if caption.is_empty() {
                "Completed".to_string()
            } else {
                caption
            };

            // Update the tool detail panel with the complete output without adding a new header
            // This keeps the original header in place to be updated by tool_complete
            self.tui.update_tool_detail(tool_name, &content);

            // Determine success based on whether there's an error in the output
            // This is a simple heuristic - you might want to make this more sophisticated
            let success = !content.contains("error")
                && !content.contains("Error")
                && !content.contains("ERROR");

            // Send the completion status to update the header
            self.tui
                .tool_complete(tool_name, success, duration_ms, &caption);
        }

        // Clear the buffers
        *self.current_tool_name.lock().unwrap() = None;
        self.current_tool_output.lock().unwrap().clear();
        *self.current_tool_start.lock().unwrap() = None;
        *self.current_tool_caption.lock().unwrap() = String::new();
    }

    fn print_agent_prompt(&self) {
        self.tui.output("\n💬 ");
    }

    fn print_agent_response(&self, content: &str) {
        self.tui.output(content);
    }

    fn notify_sse_received(&self) {
        // Notify the TUI that an SSE was received
        self.tui.sse_received();
    }

    fn flush(&self) {
        // No-op for TUI since it handles its own rendering
    }
}
