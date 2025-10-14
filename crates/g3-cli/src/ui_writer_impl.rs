use crate::retro_tui::RetroTui;
use g3_core::ui_writer::UiWriter;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::Instant;

/// Console implementation of UiWriter that prints to stdout
pub struct ConsoleUiWriter {
    current_tool_name: Mutex<Option<String>>,
    current_tool_args: Mutex<Vec<(String, String)>>,
    current_output_line: Mutex<Option<String>>,
    output_line_printed: Mutex<bool>,
}

impl ConsoleUiWriter {
    pub fn new() -> Self {
        Self {
            current_tool_name: Mutex::new(None),
            current_tool_args: Mutex::new(Vec::new()),
            current_output_line: Mutex::new(None),
            output_line_printed: Mutex::new(false),
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
        // Filter out any keys that look like they might be agent message content
        // (e.g., keys that are suspiciously long or contain message-like content)
        let is_valid_arg_key = key.len() < 50
            && !key.contains('\n')
            && !key.contains("I'll")
            && !key.contains("Let me")
            && !key.contains("Here's")
            && !key.contains("I can");

        if is_valid_arg_key {
            self.current_tool_args
                .lock()
                .unwrap()
                .push((key.to_string(), value.to_string()));
        }
    }

    fn print_tool_output_header(&self) {
        println!();
        // Now print the tool header with the most important arg in bold green
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            let args = self.current_tool_args.lock().unwrap();

            // Find the most important argument - prioritize file_path if available
            let important_arg = args
                .iter()
                .find(|(k, _)| k == "file_path")
                .or_else(|| args.iter().find(|(k, _)| k == "command" || k == "path"))
                .or_else(|| args.first());

            if let Some((_, value)) = important_arg {
                // For multi-line values, only show the first line
                let first_line = value.lines().next().unwrap_or("");

                // Truncate long values for display
                let display_value = if first_line.len() > 80 {
                    format!("{}...", &first_line[..77])
                } else {
                    first_line.to_string()
                };

                // Print with bold green formatting using ANSI escape codes
                println!("┌─\x1b[1;32m {} | {}\x1b[0m", tool_name, display_value);
            } else {
                // Print with bold green formatting using ANSI escape codes
                println!("┌─\x1b[1;32m {}\x1b[0m", tool_name);
            }
        }
    }

    fn update_tool_output_line(&self, line: &str) {
        let mut current_line = self.current_output_line.lock().unwrap();
        let mut line_printed = self.output_line_printed.lock().unwrap();

        // If we've already printed a line, clear it first
        if *line_printed {
            // Move cursor up one line and clear it
            print!("\x1b[1A\x1b[2K");
        }

        // Print the new line
        println!("│ \x1b[2m{}\x1b[0m", line);
        let _ = io::stdout().flush();

        // Update state
        *current_line = Some(line.to_string());
        *line_printed = true;
    }

    fn print_tool_output_line(&self, line: &str) {
        println!("│ \x1b[2m{}\x1b[0m", line);
    }

    fn print_tool_output_summary(&self, count: usize) {
        println!(
            "│ \x1b[2m({} line{})\x1b[0m",
            count,
            if count == 1 { "" } else { "s" }
        );
    }

    fn print_tool_timing(&self, duration_str: &str) {
        println!("└─ ⚡️ {}", duration_str);
        println!();
        // Clear the stored tool info
        *self.current_tool_name.lock().unwrap() = None;
        self.current_tool_args.lock().unwrap().clear();
        *self.current_output_line.lock().unwrap() = None;
        *self.output_line_printed.lock().unwrap() = false;
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
        // Filter out any keys that look like they might be agent message content
        // (e.g., keys that are suspiciously long or contain message-like content)
        let is_valid_arg_key = key.len() < 50
            && !key.contains('\n')
            && !key.contains("I'll")
            && !key.contains("Let me")
            && !key.contains("Here's")
            && !key.contains("I can");

        if is_valid_arg_key {
            self.current_tool_output
                .lock()
                .unwrap()
                .push(format!("{}: {}", key, value));
        }

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

    fn update_tool_output_line(&self, line: &str) {
        // For retro mode, we'll just add to the output buffer
        self.current_tool_output
            .lock()
            .unwrap()
            .push(line.to_string());
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
