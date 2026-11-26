use crate::filter_json::{filter_json_tool_calls, reset_json_tool_state, ToolParsingHint};
use crate::display::{shorten_path, shorten_paths_in_command};
use crate::streaming_markdown::StreamingMarkdownFormatter;
use crate::terminal_width::{get_terminal_width, clip_line, compress_path, compress_command};
use g3_core::ui_writer::UiWriter;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU8, Ordering}};
use termimad::MadSkin;

/// Padding width for tool names in compact display (longest tool: "str_replace" = 11 chars)
const TOOL_NAME_PADDING: usize = 11;

/// ANSI escape codes
mod ansi {
    pub const YELLOW: &str = "\x1b[33m";
    pub const ORANGE: &str = "\x1b[38;5;208m";
    pub const RED: &str = "\x1b[31m";
}

/// Colorize a str_replace summary (e.g., "+5 | -3" -> green "+5" | red "-3")
fn colorize_str_replace_summary(summary: &str) -> String {
    // Parse patterns like "+5 | -3", "+5", "-3"
    if summary.contains(" | ") {
        let parts: Vec<&str> = summary.split(" | ").collect();
        if parts.len() == 2 {
            return format!("\x1b[32m{}\x1b[0m \x1b[2m|\x1b[0m \x1b[31m{}\x1b[0m", parts[0], parts[1]);
        }
    } else if summary.starts_with('+') {
        return format!("\x1b[32m{}\x1b[0m", summary);
    } else if summary.starts_with('-') {
        return format!("\x1b[31m{}\x1b[0m", summary);
    }
    summary.to_string()
}

/// ANSI color codes for tool names
const TOOL_COLOR_NORMAL_BOLD: &str = "\x1b[1;32m";
const TOOL_COLOR_AGENT_BOLD: &str = "\x1b[1;38;5;250m";

/// Blink state values for the streaming indicator
const BLINK_INACTIVE: u8 = 0;
const BLINK_SHOW_PIPE: u8 = 1;
const BLINK_SHOW_SPACE: u8 = 2;

/// Shared state for tool parsing hints that can be used in callbacks.
/// This is separate from ConsoleUiWriter so it can be captured by Arc in closures.
#[derive(Clone)]
struct ParsingHintState {
    parsing_indicator_printed: Arc<AtomicBool>,
    last_output_was_text: Arc<AtomicBool>,
    last_output_was_tool: Arc<AtomicBool>,
    is_agent_mode: Arc<AtomicBool>,
    /// Blink state: 0 = inactive, 1 = show pipe, 2 = show space
    blink_state: Arc<AtomicU8>,
}

impl ParsingHintState {
    fn new() -> Self {
        Self {
            parsing_indicator_printed: Arc::new(AtomicBool::new(false)),
            last_output_was_text: Arc::new(AtomicBool::new(false)),
            last_output_was_tool: Arc::new(AtomicBool::new(false)),
            is_agent_mode: Arc::new(AtomicBool::new(false)),
            blink_state: Arc::new(AtomicU8::new(BLINK_INACTIVE)),
        }
    }

    fn clear(&self) {
        self.parsing_indicator_printed.store(false, Ordering::Relaxed);
        self.blink_state.store(BLINK_INACTIVE, Ordering::Relaxed);
    }

    /// Handle a tool parsing hint - this is the core logic extracted for use in callbacks
    fn handle_hint(&self, hint: ToolParsingHint) {
        match hint {
            ToolParsingHint::Detected(tool_name) => {
                // Stop any previous blinking
                self.blink_state.store(BLINK_INACTIVE, Ordering::Relaxed);
                
                // Check if we've already printed an indicator (this is an update)
                let already_printed = self.parsing_indicator_printed.load(Ordering::Relaxed);
                
                if already_printed {
                    // Update in place: clear line and reprint with new name
                    print!("\r\x1b[2K");
                } else {
                    // First time: add blank line if last output was text
                    if self.last_output_was_text.load(Ordering::Relaxed) {
                        println!();
                    }
                    self.last_output_was_text.store(false, Ordering::Relaxed);
                    self.last_output_was_tool.store(true, Ordering::Relaxed);
                }

                // Get color based on agent mode
                let tool_color = if self.is_agent_mode.load(Ordering::Relaxed) {
                    TOOL_COLOR_AGENT_BOLD
                } else {
                    TOOL_COLOR_NORMAL_BOLD
                };
                
                // Print the indicator: " ● tool_name |"
                print!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m \x1b[2m|\x1b[0m", tool_color, tool_name, width = TOOL_NAME_PADDING);
                let _ = io::stdout().flush();
                
                self.parsing_indicator_printed.store(true, Ordering::Relaxed);
                self.blink_state.store(BLINK_SHOW_PIPE, Ordering::Relaxed);
            }
            ToolParsingHint::Active => {
                // Toggle blink state for visual feedback
                let current = self.blink_state.load(Ordering::Relaxed);
                if current != BLINK_INACTIVE {
                    let new_state = if current == BLINK_SHOW_PIPE { BLINK_SHOW_SPACE } else { BLINK_SHOW_PIPE };
                    self.blink_state.store(new_state, Ordering::Relaxed);
                    let indicator = if new_state == BLINK_SHOW_PIPE { "|" } else { " " };
                    // Move back one char and reprint
                    print!("\x1b[1D\x1b[2m{}\x1b[0m", indicator);
                    let _ = io::stdout().flush();
                }
            }
            ToolParsingHint::Complete => {
                // Stop blinking
                self.blink_state.store(BLINK_INACTIVE, Ordering::Relaxed);
                // Clear the parsing indicator line - the actual tool output will follow
                if self.parsing_indicator_printed.load(Ordering::Relaxed) {
                    // Clear the current line and move to start
                    print!("\r\x1b[2K");
                    let _ = io::stdout().flush();
                }
                self.clear();
            }
        }
    }
}

/// Console implementation of UiWriter that prints to stdout
pub struct ConsoleUiWriter {
    current_tool_name: std::sync::Mutex<Option<String>>,
    current_tool_args: std::sync::Mutex<Vec<(String, String)>>,
    /// Workspace path for shortening displayed paths
    workspace_path: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// Project path for shortening displayed paths (takes priority over workspace)
    project_path: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// Project name for display (e.g., "appa_estate")
    project_name: std::sync::Mutex<Option<String>>,
    current_output_line: std::sync::Mutex<Option<String>>,
    output_line_printed: std::sync::Mutex<bool>,
    /// Track if we're in shell compact mode (for appending timing to output line)
    is_shell_compact: std::sync::Mutex<bool>,
    /// Streaming markdown formatter for agent responses
    markdown_formatter: Mutex<Option<StreamingMarkdownFormatter>>,
    /// Track the last read_file path for continuation display
    last_read_file_path: std::sync::Mutex<Option<String>>,
    /// Shared state for tool parsing hints (used by real-time callback)
    hint_state: ParsingHintState,
}

/// ANSI color code for duration display based on elapsed time.
/// Returns empty string for fast operations, yellow/orange/red for slower ones.
fn duration_color(duration_str: &str) -> &'static str {
    if duration_str.ends_with("ms") {
        return "";
    }

    if let Some(m_pos) = duration_str.find('m') {
        if let Ok(minutes) = duration_str[..m_pos].trim().parse::<u32>() {
            return match minutes {
                5.. => ansi::RED,
                1.. => ansi::ORANGE,
                _ => "",
            };
        }
    } else if let Some(s_value) = duration_str.strip_suffix('s') {
        if let Ok(seconds) = s_value.trim().parse::<f64>() {
            if seconds >= 1.0 {
                return ansi::YELLOW;
            }
        }
    }

    ""
}

impl ConsoleUiWriter {
    /// Clear all stored tool state after output is complete.
    fn clear_tool_state(&self) {
        *self.current_tool_name.lock().unwrap() = None;
        self.current_tool_args.lock().unwrap().clear();
        *self.current_output_line.lock().unwrap() = None;
        *self.output_line_printed.lock().unwrap() = false;
    }

}

impl ConsoleUiWriter {
    pub fn new() -> Self {
        Self {
            current_tool_name: std::sync::Mutex::new(None),
            current_tool_args: std::sync::Mutex::new(Vec::new()),
            workspace_path: std::sync::Mutex::new(None),
            project_path: std::sync::Mutex::new(None),
            project_name: std::sync::Mutex::new(None),
            current_output_line: std::sync::Mutex::new(None),
            output_line_printed: std::sync::Mutex::new(false),
            is_shell_compact: std::sync::Mutex::new(false),
            markdown_formatter: Mutex::new(None),
            last_read_file_path: std::sync::Mutex::new(None),
            hint_state: ParsingHintState::new(),
        }
    }
}

impl ConsoleUiWriter {
    fn get_workspace_path(&self) -> Option<std::path::PathBuf> {
        self.workspace_path.lock().unwrap().clone()
    }

    fn get_project_info(&self) -> Option<(std::path::PathBuf, String)> {
        let path = self.project_path.lock().unwrap().clone()?;
        let name = self.project_name.lock().unwrap().clone()?;
        Some((path, name))
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

    fn print_g3_progress(&self, message: &str) {
        crate::g3_status::G3Status::progress(message);
    }

    fn print_g3_status(&self, message: &str, status: &str) {
        use crate::g3_status::Status;
        let _ = message; // unused now - progress already printed the message
        crate::g3_status::G3Status::status(&Status::parse(status));
    }

    fn print_thin_result(&self, result: &g3_core::ThinResult) {
        // Use centralized G3Status formatting
        crate::g3_status::G3Status::thin_result(result);
    }

    fn print_context_summary(&self, used_tokens: u32, total_tokens: u32, percentage: f32) {
        // Reuse the same dot-bar rendering as the interactive prompt bar.
        crate::utils::render_context_bar(used_tokens, total_tokens, percentage);
    }

    fn print_tool_header(&self, tool_name: &str, _tool_args: Option<&serde_json::Value>) {
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
        // Clear any streaming hint that might be showing
        // This ensures we don't duplicate the tool name on the line
        self.hint_state.handle_hint(ToolParsingHint::Complete);

        // Add blank line if last output was text (for visual separation)
        let last_was_text = self.hint_state.last_output_was_text.load(Ordering::Relaxed);
        if last_was_text {
            println!();
        }
        self.hint_state.last_output_was_text.store(false, Ordering::Relaxed);
        self.hint_state.last_output_was_tool.store(true, Ordering::Relaxed);

        // Reset output_line_printed at the start of a new tool output
        // This ensures the header isn't cleared by update_tool_output_line
        *self.output_line_printed.lock().unwrap() = false;
        // Reset shell compact mode
        *self.is_shell_compact.lock().unwrap() = false;
        // Now print the tool header with the most important arg
        // Use light gray/silver in agent mode, bold green otherwise
        let is_agent_mode = self.hint_state.is_agent_mode.load(Ordering::Relaxed);
        // Light gray/silver: \x1b[38;5;250m, Bold green: \x1b[1;32m
        let tool_color = if is_agent_mode {
            TOOL_COLOR_AGENT_BOLD
        } else {
            TOOL_COLOR_NORMAL_BOLD
        };

        // Get terminal width for responsive formatting
        let term_width = get_terminal_width();

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

                // Get workspace path for shortening
                let workspace = self.get_workspace_path();
                let workspace_ref = workspace.as_deref();
                
                // Get project info for shortening
                let project_info = self.get_project_info();
                let project_ref = project_info.as_ref().map(|(p, n)| (p.as_path(), n.as_str()));

                // Shorten paths in the value (handles both file paths and shell commands)
                let shortened = shorten_paths_in_command(first_line, workspace_ref, project_ref);

                // Build range suffix for read_file FIRST so we can account for its width
                let header_suffix = if tool_name == "read_file" {
                    // Check if start or end parameters are present
                    let has_start = args.iter().any(|(k, _)| k == "start");
                    let has_end = args.iter().any(|(k, _)| k == "end");

                    if has_start || has_end {
                        let start_val = args
                            .iter()
                            .find(|(k, _)| k == "start")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("0");
                        let end_val = args
                            .iter()
                            .find(|(k, _)| k == "end")
                            .map(|(_, v)| v.as_str())
                            .unwrap_or("end");
                        format!(" [{}..{}]", start_val, end_val)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Calculate available width for the value
                // Header format: "┌─<tool_color> <tool_name><reset><magenta> | <value><suffix><reset>"
                // Prefix overhead: "┌─" (2) + tool_name + " | " (3) = 5 + tool_name.len()
                // For shell: " ●  <tool_name>  | " = ~17 chars overhead
                let is_shell_tool = tool_name == "shell";
                let prefix_overhead = if is_shell_tool { 17 } else { 5 + tool_name.len() };
                // Subtract suffix length from available width
                let available_for_value = term_width.saturating_sub(prefix_overhead + header_suffix.chars().count());

                // Compress path or command to fit available width
                let display_value = if is_shell_tool || tool_name == "background_process" {
                    compress_command(&shortened, available_for_value)
                } else {
                    compress_path(&shortened, available_for_value)
                };

                // Check if this is a shell command - use compact format
                if tool_name == "shell" {
                    *self.is_shell_compact.lock().unwrap() = true;
                    // Print compact shell header: "● shell      | command"
                    // Pad to align with longest compact tool (str_replace = 11 chars)
                    println!(
                        " \x1b[2m●\x1b[0m {}{:<11}\x1b[0m \x1b[2m|\x1b[0m \x1b[35m{}\x1b[0m",
                        tool_color, tool_name, display_value
                    );
                    return;
                }

                // Print with tool name in color (royal blue for agent mode, green otherwise)
                println!(
                    "┌─{} {}\x1b[0m\x1b[35m | {}{}\x1b[0m",
                    tool_color, tool_name, display_value, header_suffix
                );
            } else {
                // Print with tool name in color
                println!("┌─{} {}\x1b[0m", tool_color, tool_name);
            }
        }
    }

    fn update_tool_output_line(&self, line: &str) {
        // Get terminal width and calculate available space for content
        // Prefix is "│ " (3 chars) for normal tools or "   └─ " (6 chars) for shell
        let mut current_line = self.current_output_line.lock().unwrap();
        let mut line_printed = self.output_line_printed.lock().unwrap();
        let is_shell = *self.is_shell_compact.lock().unwrap();
        let prefix_width = if is_shell { 6 } else { 3 };
        // For shell, reserve space for suffix: " (N lines) | N ◉ Xms"
        // - " (9999 lines)" = 13 chars max
        // - " | 99999 ◉ 999ms" = 17 chars max
        // Total suffix overhead: ~30 chars
        let suffix_overhead = if is_shell { 30 } else { 0 };
        let max_content_width = get_terminal_width()
            .saturating_sub(prefix_width)
            .saturating_sub(suffix_overhead);

        // If we've already printed a line, clear it first
        if *line_printed {
            if is_shell {
                // For shell, we printed without newline, so just clear the line
                print!("\r\x1b[2K");
            } else {
                // Move cursor up one line and clear it
                print!("\x1b[1A\x1b[2K");
            }
        }

        // Clip line to fit terminal width
        let display_line = clip_line(line, max_content_width);

        // Use different prefix for shell (└─) vs other tools (│)
        if is_shell {
            // For shell, print without newline so timing can be appended
            print!("   \x1b[2m└─ {}\x1b[0m", display_line);
        } else {
            println!("│ \x1b[2m{}\x1b[0m", display_line);
        }
        let _ = io::stdout().flush();

        // Update state
        *current_line = Some(line.to_string());
        *line_printed = true;
    }

    fn print_tool_output_line(&self, line: &str) {
        // Skip the TODO list header line
        if line.starts_with("📝 TODO list:") {
            return;
        }
        // Clip line to fit terminal width (prefix "│ " is 3 chars)
        let max_content_width = get_terminal_width().saturating_sub(3);
        println!("│ \x1b[2m{}\x1b[0m", clip_line(line, max_content_width));
    }

    fn print_tool_output_summary(&self, count: usize) {
        let is_shell = *self.is_shell_compact.lock().unwrap();
        if is_shell {
            // For shell, append to the same line (no newline)
            print!(" \x1b[2m({} line{})\x1b[0m", count, if count == 1 { "" } else { "s" });
            let _ = io::stdout().flush();
        } else {
            println!(
                "│ \x1b[2m({} line{})\x1b[0m",
                count,
                if count == 1 { "" } else { "s" }
            );
        }
    }

    fn print_tool_compact(&self, tool_name: &str, summary: &str, duration_str: &str, tokens_delta: u32, _context_percentage: f32) -> bool {
        // Clear any streaming hint that might be showing
        // This ensures we don't duplicate the tool name on the line
        self.hint_state.handle_hint(ToolParsingHint::Complete);

        // Handle file operation tools and other compact tools
        let is_compact_tool = matches!(tool_name, "read_file" | "write_file" | "str_replace" | "remember" | "screenshot" | "coverage" | "code_search" | "load_toolset");
        if !is_compact_tool {
            // Reset continuation tracking for non-compact tools
            *self.last_read_file_path.lock().unwrap() = None;
            return false;
        }

        // Add blank line if last output was text (for visual separation)
        if self.hint_state.last_output_was_text.load(Ordering::Relaxed) {
            println!();
        }
        self.hint_state.last_output_was_text.store(false, Ordering::Relaxed);
        self.hint_state.last_output_was_tool.store(true, Ordering::Relaxed);

        let args = self.current_tool_args.lock().unwrap();
        let is_agent_mode = self.hint_state.is_agent_mode.load(Ordering::Relaxed);

        // Get terminal width for responsive formatting
        let term_width = get_terminal_width();

        // Get file path (for file operation tools)
        let file_path = args
            .iter()
            .find(|(k, _)| k == "file_path")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");

        // Check if this is a continuation of reading the same file
        let mut last_read_path = self.last_read_file_path.lock().unwrap();
        let is_continuation = tool_name == "read_file" && !file_path.is_empty() && last_read_path.as_deref() == Some(file_path);

        // For tools without file_path, get other relevant args
        let display_arg = if file_path.is_empty() {
            // For code_search, extract language and name from searches
            if tool_name == "code_search" {
                // searches arg is JSON array, try to extract first search's language and name
                if let Some((_, searches_json)) = args.iter().find(|(k, _)| k == "searches") {
                    if let Ok(searches) = serde_json::from_str::<serde_json::Value>(searches_json) {
                        if let Some(first_search) = searches.as_array().and_then(|arr| arr.first()) {
                            let lang = first_search.get("language").and_then(|v| v.as_str()).unwrap_or("?");
                            let name = first_search.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            // Calculate available width for search name
                            // Format: " ● code_search | lang:"name" | summary | tokens ◉ time"
                            // Fixed overhead: ~50 chars + lang (~10) = ~60
                            let available_for_name = term_width.saturating_sub(60);
                            let display_name = clip_line(name, available_for_name);
                            format!("{}:\"{}\"", lang, display_name)
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                // For load_toolset, show the toolset name
                if tool_name == "load_toolset" {
                    args.iter().find(|(k, _)| k == "name")
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default()
                } else {
                    // For remember, screenshot, etc. - no path to show
                    String::new()
                }
            }
        } else {
            // Shorten path (project -> name/, workspace -> ./, home -> ~) then truncate if still long
            let workspace = self.get_workspace_path();
            let project_info = self.get_project_info();
            let project_ref = project_info.as_ref().map(|(p, n)| (p.as_path(), n.as_str()));
            let shortened = shorten_path(file_path, workspace.as_deref(), project_ref);

            // Calculate available width for path
            // Format: " ● tool_name   | path [range] | summary | tokens ◉ time"
            // Fixed overhead: " ● " (3) + tool_name padded (11) + " | " (3) + " | " (3) + summary (~15) + " | " (3) + tokens+time (~15) = ~53
            // Plus range_suffix length (variable, ~10-15 chars if present)
            let fixed_overhead = 53;
            let available_for_path = term_width.saturating_sub(fixed_overhead);
            compress_path(&shortened, available_for_path)
        };

        // Build range suffix for read_file
        let range_suffix = if tool_name == "read_file" {
            let has_start = args.iter().any(|(k, _)| k == "start");
            let has_end = args.iter().any(|(k, _)| k == "end");
            if has_start || has_end {
                let start_val = args
                    .iter()
                    .find(|(k, _)| k == "start")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("0");
                let end_val = args
                    .iter()
                    .find(|(k, _)| k == "end")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("end");
                format!(" [{}..{}]", start_val, end_val)
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Color for tool name
        let tool_color = if is_agent_mode { TOOL_COLOR_AGENT_BOLD } else { TOOL_COLOR_NORMAL_BOLD };

        // Colorize summary for str_replace (green insertions, red deletions)
        let display_summary = if tool_name == "str_replace" {
            colorize_str_replace_summary(summary)
        } else {
            summary.to_string()
        };

        // Calculate available width for summary based on line format
        // Continuation: "   └─ reading further" (21) + range + " | " (3) + summary + " | " (3) + tokens+time (~15) = ~42 + range
        // No path: " ● " (3) + tool_name (11) + " | " (3) + summary + " | " (3) + tokens+time (~15) = ~35
        // With path: " ● " (3) + tool_name (11) + " | " (3) + path + range + " | " (3) + summary + " | " (3) + tokens+time (~15)
        let tokens_time_overhead = 3 + format!("{}", tokens_delta).len() + 3 + duration_str.len(); // " | N ◉ Xs"
        let summary_available = if is_continuation {
            term_width.saturating_sub(42 + range_suffix.chars().count() + tokens_time_overhead)
        } else if display_arg.is_empty() {
            term_width.saturating_sub(35 + tokens_time_overhead)
        } else {
            term_width.saturating_sub(35 + display_arg.chars().count() + range_suffix.chars().count() + tokens_time_overhead)
        };
        let display_summary = clip_line(&display_summary, summary_available);

        // Print compact single line
        if is_continuation {
            // Continuation line for consecutive read_file on same file:
            // "   └─ reading further [range] | summary | tokens ◉ time"
            println!(
                "   \x1b[2m└─ reading further\x1b[0m\x1b[35m{}\x1b[0m \x1b[2m| {}\x1b[0m \x1b[2m| {} ◉ {}\x1b[0m",
                range_suffix,
                display_summary,
                tokens_delta,
                duration_str
            );
        } else if display_arg.is_empty() {
            // Tools without file path: " ● tool_name | summary | tokens ◉ time"
            // Pad to align with longest compact tool (str_replace = 11 chars)
            println!(
                " \x1b[2m●\x1b[0m {}{:<11}\x1b[0m \x1b[2m| {}\x1b[0m \x1b[2m| {} ◉ {}\x1b[0m",
                tool_color, tool_name, display_summary, tokens_delta, duration_str
            );
        } else {
            // Tools with file path: " ● tool_name | path [range] | summary | tokens ◉ time"
            // Pad to align with longest compact tool (str_replace = 11 chars)
            println!(
                " \x1b[2m●\x1b[0m {}{:<11}\x1b[0m \x1b[2m|\x1b[0m \x1b[35m{}{}\x1b[0m \x1b[2m| {}\x1b[0m \x1b[2m| {} ◉ {}\x1b[0m",
                tool_color, tool_name, display_arg, range_suffix, display_summary, tokens_delta, duration_str
            );
        }

        // Update last_read_file_path for continuation tracking
        if tool_name == "read_file" && !file_path.is_empty() {
            *last_read_path = Some(file_path.to_string());
        } else {
            // Reset for non-read_file tools
            *last_read_path = None;
        }

        // Clear the stored tool info
        drop(args); // Release the lock before clearing
        drop(last_read_path); // Release this lock too
        self.clear_tool_state();

        true
    }

    fn print_todo_compact(&self, content: Option<&str>, is_write: bool) -> bool {
        let tool_name = if is_write { "todo_write" } else { "todo_read" };
        // Clear any streaming hint that might be showing
        // This ensures we don't duplicate the tool name on the line
        self.hint_state.handle_hint(ToolParsingHint::Complete);

        let is_agent_mode = self.hint_state.is_agent_mode.load(Ordering::Relaxed);
        let tool_color = if is_agent_mode { TOOL_COLOR_AGENT_BOLD } else { TOOL_COLOR_NORMAL_BOLD };

        // Add blank line if last output was text (for visual separation)
        if self.hint_state.last_output_was_text.load(Ordering::Relaxed) {
            println!();
        }
        self.hint_state.last_output_was_text.store(false, Ordering::Relaxed);
        self.hint_state.last_output_was_tool.store(true, Ordering::Relaxed);
        // Reset read_file continuation tracking
        *self.last_read_file_path.lock().unwrap() = None;

        match content {
            None => {
                // Empty TODO
                println!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m \x1b[2m|\x1b[0m \x1b[35mempty\x1b[0m", tool_color, tool_name, width = TOOL_NAME_PADDING);
            }
            Some(text) => {
                // Header
                println!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m", tool_color, tool_name, width = TOOL_NAME_PADDING);
                
                let lines: Vec<&str> = text.lines().collect();
                let last_idx = lines.len().saturating_sub(1);
                
                for (i, line) in lines.iter().enumerate() {
                    let is_last = i == last_idx;
                    let prefix = if is_last { "└" } else { "│" };
                    
                    // Convert checkboxes to styled symbols and strikethrough completed items
                    let is_completed = line.contains("- [x]") || line.contains("- [X]");
                    let styled_line = if is_completed {
                        // Replace checkbox and apply strikethrough to the task text
                        let task_text = line
                            .replace("- [x]", "")
                            .replace("- [X]", "")
                            .trim_start()
                            .to_string();
                        format!("■ \x1b[9m{}\x1b[0m\x1b[2m", task_text)  // \x1b[9m is strikethrough
                    } else {
                        line.replace("- [ ]", "□")
                    };

                    // Clip line to fit terminal width (prefix "   X  " is 6 chars)
                    let max_content_width = get_terminal_width().saturating_sub(6);
                    let clipped_line = clip_line(&styled_line, max_content_width);
                    // Dim the line content
                    println!("   \x1b[2m{}  {}\x1b[0m", prefix, clipped_line);
                }
                // Add blank line after content for readability
                println!();
            }
        }

        // Clear tool state
        self.clear_tool_state();
        
        true
    }

    fn print_plan_compact(&self, plan_yaml: Option<&str>, plan_file_path: Option<&str>, is_write: bool) -> bool {
        let tool_name = if is_write { "plan_write" } else { "plan_read" };
        // Clear any streaming hint that might be showing
        self.hint_state.handle_hint(ToolParsingHint::Complete);

        let is_agent_mode = self.hint_state.is_agent_mode.load(Ordering::Relaxed);
        let tool_color = if is_agent_mode { TOOL_COLOR_AGENT_BOLD } else { TOOL_COLOR_NORMAL_BOLD };

        // Add blank line if last output was text (for visual separation)
        if self.hint_state.last_output_was_text.load(Ordering::Relaxed) {
            println!();
        }
        self.hint_state.last_output_was_text.store(false, Ordering::Relaxed);
        self.hint_state.last_output_was_tool.store(true, Ordering::Relaxed);
        // Reset read_file continuation tracking
        *self.last_read_file_path.lock().unwrap() = None;

        for line in classic_plan_lines(plan_yaml, plan_file_path, tool_name, tool_color, get_terminal_width()) {
            println!("{}", line);
        }

        // Clear tool state
        self.clear_tool_state();

        true
    }

    fn print_envelope_compact(&self, fact_groups: usize, stages: &[(&str, &str)], passed: Option<usize>, total: Option<usize>, failed: usize) {
        // Clear any streaming hint
        self.hint_state.handle_hint(ToolParsingHint::Complete);

        let is_agent_mode = self.hint_state.is_agent_mode.load(Ordering::Relaxed);
        let tool_color = if is_agent_mode { TOOL_COLOR_AGENT_BOLD } else { TOOL_COLOR_NORMAL_BOLD };

        // Add blank line if last output was text
        if self.hint_state.last_output_was_text.load(Ordering::Relaxed) {
            println!();
        }
        self.hint_state.last_output_was_text.store(false, Ordering::Relaxed);
        self.hint_state.last_output_was_tool.store(true, Ordering::Relaxed);
        *self.last_read_file_path.lock().unwrap() = None;

        // Header: " ● write_envelope | N fact groups"
        let facts_label = if fact_groups == 1 { "fact group" } else { "fact groups" };
        println!(
            " \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m \x1b[2m|\x1b[0m \x1b[35m{} {}\x1b[0m",
            tool_color, "write_envelope", fact_groups, facts_label, width = TOOL_NAME_PADDING
        );

        // Pipeline stages
        let stages_len = stages.len();
        // Determine if we need to show a verification summary line after stages
        let has_verification = passed.is_some();

        for (i, (icon, desc)) in stages.iter().enumerate() {
            let is_last = i == stages_len - 1 && !has_verification;
            let prefix = if is_last { "└" } else { "├" };
            println!("   \x1b[2m{}\x1b[0m {} \x1b[2m{}\x1b[0m", prefix, icon, desc);
        }

        // Verification summary line (if rulespec was present)
        if let (Some(p), Some(t)) = (passed, total) {
            if failed == 0 {
                println!(
                    "   \x1b[2m└\x1b[0m \x1b[32m✓ {}/{} passed\x1b[0m",
                    p, t
                );
            } else {
                println!(
                    "   \x1b[2m└\x1b[0m \x1b[31m✗ {}/{} passed, {} failed\x1b[0m",
                    p, t, failed
                );
            }
        }

        println!();

        // Clear tool state
        self.clear_tool_state();
    }

    fn print_tool_timing(&self, duration_str: &str, tokens_delta: u32, context_percentage: f32) {
        let color_code = duration_color(duration_str);

        // Reset read_file continuation tracking for non-read_file tools
        // (read_file tools handle this in print_tool_compact)
        if let Some(tool_name) = self.current_tool_name.lock().unwrap().as_ref() {
            if tool_name != "read_file" {
                *self.last_read_file_path.lock().unwrap() = None;
            }
        }

        // Check if we're in shell compact mode - append timing to the output line
        let is_shell = *self.is_shell_compact.lock().unwrap();
        if is_shell {
            // Append timing to the same line as shell output
            println!(" \x1b[2m| {} ◉ {}{}\x1b[0m", tokens_delta, color_code, duration_str);
            println!();
        } else {
            println!("└─ ⚡️ {}{}\x1b[0m  \x1b[2m{} ◉ | {:.0}%\x1b[0m", color_code, duration_str, tokens_delta, context_percentage);
            println!();
        }
        
        // Clear the stored tool info
        self.clear_tool_state();
        *self.is_shell_compact.lock().unwrap() = false;
    }

    fn print_agent_prompt(&self) {
        let _ = io::stdout().flush();
    }

    fn print_agent_response(&self, content: &str) {
        let mut formatter_guard = self.markdown_formatter.lock().unwrap();
        
        // Initialize formatter if not already done
        if formatter_guard.is_none() {
            let mut skin = MadSkin::default();
            skin.bold.set_fg(termimad::crossterm::style::Color::Green);
            skin.italic.set_fg(termimad::crossterm::style::Color::Cyan);
            skin.inline_code.set_fg(termimad::crossterm::style::Color::Rgb { r: 216, g: 177, b: 114 });
            *formatter_guard = Some(StreamingMarkdownFormatter::new(skin));
        }
        
        // Process the chunk through the formatter
        if let Some(ref mut formatter) = *formatter_guard {
            // Add blank line if last output was a tool call (for visual separation)
            // Only do this once at the start of new text content
            let last_was_tool = self.hint_state.last_output_was_tool.load(Ordering::Relaxed);
            if last_was_tool && !content.trim().is_empty() {
                println!();
                self.hint_state.last_output_was_tool.store(false, Ordering::Relaxed);
            }

            let formatted = formatter.process(content);
            print!("{}", formatted);
            // Track that we just output text (only if non-empty)
            if !content.trim().is_empty() {
                self.hint_state.last_output_was_text.store(true, Ordering::Relaxed);
                // Reset read_file continuation tracking when text is output between tool calls
                *self.last_read_file_path.lock().unwrap() = None;
            }
            let _ = io::stdout().flush();
        }
    }

    fn finish_streaming_markdown(&self) {
        let mut formatter_guard = self.markdown_formatter.lock().unwrap();
        
        if let Some(ref mut formatter) = *formatter_guard {
            // Flush any remaining buffered content
            let remaining = formatter.finish();
            print!("{}", remaining);
            let _ = io::stdout().flush();
        }
        
        // Reset the formatter for the next response
        *formatter_guard = None;
    }

    fn notify_sse_received(&self) {
        // No-op for console - we don't track SSEs in console mode
    }

    fn print_tool_streaming_hint(&self, tool_name: &str) {
        // Use the hint state to show the streaming indicator
        self.hint_state.handle_hint(ToolParsingHint::Detected(tool_name.to_string()));
    }

    fn print_tool_streaming_active(&self) {
        // Trigger the blink animation
        self.hint_state.handle_hint(ToolParsingHint::Active);
    }

    fn flush(&self) {
        let _ = io::stdout().flush();
    }

    fn prompt_user_yes_no(&self, message: &str) -> bool {
        print!("{} [y/N] ", message);
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim().to_lowercase();
            trimmed == "y" || trimmed == "yes"
        } else {
            false
        }
    }

    fn prompt_user_choice(&self, message: &str, options: &[&str]) -> usize {
        println!("{} ", message);
        for (i, option) in options.iter().enumerate() {
            println!("  [{}] {}", i + 1, option);
        }
        print!("Select an option (1-{}): ", options.len());
        let _ = io::stdout().flush();

        loop {
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                if let Ok(choice) = input.trim().parse::<usize>() {
                    if choice > 0 && choice <= options.len() {
                        return choice - 1;
                    }
                }
            }
            print!("Invalid choice. Please select (1-{}): ", options.len());
            let _ = io::stdout().flush();
        }
    }


    fn filter_json_tool_calls(&self, content: &str) -> String {
        // Filter the content to remove JSON tool calls from display.
        // Tool streaming hints are now handled via the provider's tool_call_streaming
        // field in CompletionChunk, not via callbacks during JSON filtering.
        filter_json_tool_calls(content)
    }

    fn reset_json_filter(&self) {
        // Reset the filter state for a new response
        reset_json_tool_state();
    }

    fn set_agent_mode(&self, is_agent_mode: bool) {
        self.hint_state.is_agent_mode.store(is_agent_mode, Ordering::Relaxed);
    }

    fn set_workspace_path(&self, path: std::path::PathBuf) {
        *self.workspace_path.lock().unwrap() = Some(path);
    }

    fn set_project_path(&self, path: std::path::PathBuf, name: String) {
        *self.project_path.lock().unwrap() = Some(path);
        *self.project_name.lock().unwrap() = Some(name);
    }

    fn clear_project(&self) {
        *self.project_path.lock().unwrap() = None;
        *self.project_name.lock().unwrap() = None;
    }
}

/// Lines for the classic (non-pretty) plan tree. Pure, so it is testable —
/// the println! version was not, and that is how a slim plan (no `touches`)
/// silently fell back to a raw YAML dump on 2026-11-26.
///
/// The slim plan is `{id, description, state, notes?}`; legacy
/// `touches`/`checks`/`evidence` are ignored by serde. Every written plan is
/// approved, so no approval label.
fn classic_plan_lines(
    plan_yaml: Option<&str>,
    plan_file_path: Option<&str>,
    tool_name: &str,
    tool_color: &str,
    term_width: usize,
) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct PlanCompact {
        plan_id: String,
        items: Vec<PlanItemCompact>,
    }
    #[derive(serde::Deserialize)]
    struct PlanItemCompact {
        id: String,
        description: String,
        #[serde(default)]
        state: String,
        #[serde(default)]
        notes: Option<String>,
    }

    let mut out = Vec::new();
    let Some(yaml) = plan_yaml else {
        out.push(format!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m \x1b[2m|\x1b[0m \x1b[35mempty\x1b[0m", tool_color, tool_name, width = TOOL_NAME_PADDING));
        return out;
    };
    let Ok(plan) = serde_yaml::from_str::<PlanCompact>(yaml) else {
        // Unparseable: fall back to the raw YAML so nothing is hidden.
        out.push(format!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m", tool_color, tool_name, width = TOOL_NAME_PADDING));
        let fallback_width = term_width.saturating_sub(6); // "   │  " = 6 chars
        for line in yaml.lines().take(20) {
            out.push(format!("   \x1b[2m│  {}\x1b[0m", clip_line(line, fallback_width)));
        }
        out.push(String::new());
        return out;
    };

    let count = |st: &str| plan.items.iter().filter(|i| i.state == st).count();
    let (done, doing, blocked) = (count("done"), count("doing"), count("blocked"));
    let todo = plan.items.len() - done - doing - blocked;
    let progress_bar = format!(
        "\x1b[32m{}\x1b[33m{}\x1b[31m{}\x1b[2m{}\x1b[0m",
        "■".repeat(done), "■".repeat(doing), "■".repeat(blocked), "□".repeat(todo)
    );
    out.push(format!(" \x1b[2m●\x1b[0m {}{:<width$}\x1b[0m \x1b[2m|\x1b[0m \x1b[1;36m{}\x1b[0m \x1b[2m[{}/{}]\x1b[0m {}",
        tool_color, tool_name, plan.plan_id, done, plan.items.len(), progress_bar, width = TOOL_NAME_PADDING));

    let n = plan.items.len();
    for (i, item) in plan.items.iter().enumerate() {
        let last = i + 1 == n;
        // □ todo, ◐ doing, ■ done, ⊘ blocked
        let (icon, color) = match item.state.as_str() {
            "doing" => ("◐", "\x1b[33m"),
            "done" => ("■", "\x1b[32m"),
            "blocked" => ("⊘", "\x1b[31m"),
            "todo" | "" => ("□", "\x1b[0m"),
            _ => ("?", "\x1b[0m"),
        };
        let branch = if last { "└" } else { "├" };
        let child = if last { " " } else { "│" };
        // "   X " (5) + icon (1) + " " (1) + id + " " (1) ≈ 11 + id
        let max_desc = term_width.saturating_sub(11 + item.id.chars().count());
        let desc = clip_line(&item.description, max_desc);
        let (on, off) = if item.state == "done" { ("\x1b[9m\x1b[2m", "\x1b[0m") } else { ("", "") };
        out.push(format!("   \x1b[2m{}\x1b[0m {}{}\x1b[0m \x1b[1m{}\x1b[0m {}{}{}", branch, color, icon, item.id, on, desc, off));
        if let Some(note) = item.notes.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            // "   X    ↳ " = 10 chars
            out.push(format!("   \x1b[2m{}    ↳ {}\x1b[0m", child, clip_line(note, term_width.saturating_sub(10))));
        }
    }
    if let Some(path) = plan_file_path {
        out.push(format!("   \x1b[2m📄 {}\x1b[0m", clip_line(path, term_width.saturating_sub(5))));
    }
    out.push(String::new());
    out
}

#[cfg(test)]
mod classic_plan_tests {
    use super::classic_plan_lines;

    fn plain(lines: &[String]) -> String {
        let s = lines.join("\n");
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            if c == '\x1b' { esc = true; continue; }
            if esc { if c == 'm' { esc = false; } continue; }
            out.push(c);
        }
        out
    }

    #[test]
    fn slim_plan_renders_as_a_tree_not_a_yaml_dump() {
        let yaml = "plan_id: demo\nrevision: 2\napproved_revision: 2\nitems:\n- id: I1\n  description: first\n  state: done\n- id: I2\n  description: second\n  state: blocked\n  notes: waiting on design\n";
        let t = plain(&classic_plan_lines(Some(yaml), Some("/tmp/p.md"), "plan_write", "", 80));
        for want in ["demo", "[1/2]", "■ I1 first", "⊘ I2 second", "↳ waiting on design", "📄 /tmp/p.md"] {
            assert!(t.contains(want), "missing {want:?} in:\n{t}");
        }
        assert!(!t.contains("description:"), "fell back to raw YAML:\n{t}");
        assert!(!t.contains("approved"), "no approval label any more:\n{t}");
    }

    #[test]
    fn legacy_plan_parses_and_drops_its_ceremony() {
        let yaml = "plan_id: old\nrevision: 1\nitems:\n  - id: I1\n    description: legacy\n    state: doing\n    touches: [b]\n    checks: {happy: {desc: works, target: t}, negative: [], boundary: []}\n    evidence: [src/a.rs]\n";
        let t = plain(&classic_plan_lines(Some(yaml), None, "plan_write", "", 80));
        assert!(t.contains("◐ I1 legacy"), "{t}");
        for gone in ["works", "src/a.rs", "→", "touches:"] {
            assert!(!t.contains(gone), "{gone:?} leaked:\n{t}");
        }
    }

    #[test]
    fn unparseable_yaml_still_falls_back_and_empty_says_empty() {
        let t = plain(&classic_plan_lines(Some("not: [a plan"), None, "plan_write", "", 80));
        assert!(t.contains("│  not: [a plan"), "{t}");
        let e = plain(&classic_plan_lines(None, None, "plan_write", "", 80));
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn long_description_is_clipped_to_the_terminal() {
        let long = "x".repeat(300);
        let yaml = format!("plan_id: w\nitems:\n- {{id: I1, description: {long}, state: todo}}\n");
        let lines = classic_plan_lines(Some(&yaml), None, "plan_write", "", 40);
        let item = plain(&lines[1..2]);
        assert!(item.chars().count() <= 40, "{} chars: {item}", item.chars().count());
    }
}
