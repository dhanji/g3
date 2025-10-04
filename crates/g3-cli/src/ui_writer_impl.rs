use crate::retro_tui::RetroTui;
use g3_core::ui_writer::UiWriter;
use std::io::{self, Write};
use std::sync::Mutex;

/// Console implementation of UiWriter that prints to stdout
pub struct ConsoleUiWriter;

impl ConsoleUiWriter {
    pub fn new() -> Self {
        Self
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
        println!("┌─ {}", tool_name);
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        println!("│ {}: {}", key, value);
    }

    fn print_tool_output_header(&self) {
        println!("├─ output:");
    }

    fn print_tool_output_line(&self, line: &str) {
        println!("│ {}", line);
    }

    fn print_tool_output_summary(&self, hidden_count: usize) {
        println!(
            "│ ... ({} more line{} hidden)",
            hidden_count,
            if hidden_count == 1 { "" } else { "s" }
        );
    }

    fn print_tool_timing(&self, duration_str: &str) {
        println!("└─ ⚡️ {}", duration_str);
        println!();
    }

    fn print_agent_prompt(&self) {
        print!(" ");
        let _ = io::stdout().flush();
    }

    fn print_agent_response(&self, content: &str) {
        print!("{}", content);
        let _ = io::stdout().flush();
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
}

impl RetroTuiWriter {
    pub fn new(tui: RetroTui) -> Self {
        Self {
            tui,
            current_tool_name: Mutex::new(None),
            current_tool_output: Mutex::new(Vec::new()),
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
        *self.current_tool_name.lock().unwrap() = Some(tool_name.to_string());
        self.current_tool_output.lock().unwrap().clear();
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("Tool: {}", tool_name));
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("{}: {}", key, value));
    }

    fn print_tool_output_header(&self) {
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
            "... ({} more line{} hidden)",
            hidden_count,
            if hidden_count == 1 { "" } else { "s" }
        ));
    }

    fn print_tool_timing(&self, duration_str: &str) {
        self.current_tool_output
            .lock()
            .unwrap()
            .push(format!("⚡️ {}", duration_str));

        // Now send the complete tool output as a box
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            let content = self.current_tool_output.lock().unwrap().join("\n");
            self.tui.tool_output(tool_name, "...", &content);
        }

        // Clear the buffers
        *self.current_tool_name.lock().unwrap() = None;
        self.current_tool_output.lock().unwrap().clear();
    }

    fn print_agent_prompt(&self) {
        self.tui.output("\n💬 ");
    }

    fn print_agent_response(&self, content: &str) {
        self.tui.output(content);
    }

    fn flush(&self) {
        // No-op for TUI since it handles its own rendering
    }
}
