use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame, Terminal,
};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use std::collections::VecDeque;

use crate::theme::ColorTheme;

// Color theme will be loaded dynamically

// Scrolling configuration
const SCROLL_PAST_END_BUFFER: usize = 10; // Extra lines to allow scrolling past the end

/// Message types for communication between threads
#[derive(Debug, Clone)]
pub enum TuiMessage {
    AgentOutput(String),
    ToolOutput {
        name: String,
        caption: String,
        content: String,
    },
    ToolDetailUpdate {
        name: String,
        content: String,
    },
    ToolComplete {
        name: String,
        success: bool,
        duration_ms: u128,
        caption: String,
    },
    SystemStatus(String),
    ContextUpdate {
        used: u32,
        total: u32,
        percentage: f32,
    },
    SSEReceived, // New message type for SSE events (including pings)
    Error(String),
    Exit,
}

/// Shared state for the retro terminal
struct TerminalState {
    /// Color theme
    theme: ColorTheme,
    /// Current input buffer
    input_buffer: String,
    /// Cursor position in input buffer (for editing)
    cursor_position: usize,
    /// Output history
    output_history: Vec<String>,
    /// Scroll position in output
    scroll_offset: usize,
    /// Cursor blink state
    cursor_blink: bool,
    /// Animation state for activity area (0.0 = hidden, 1.0 = fully shown)
    activity_animation: f32,
    /// Target animation state
    activity_animation_target: f32,
    /// Tool activity history (left side of activity box)
    tool_activity: Vec<String>,
    /// Track if tool activity should auto-scroll
    tool_activity_auto_scroll: bool,
    /// Tool activity scroll offset
    tool_activity_scroll: usize,
    /// Last known visible height of output area
    last_visible_height: usize,
    /// User has manually scrolled (disable auto-scroll)
    manual_scroll: bool,
    /// Last cursor blink time
    last_blink: Instant,
    /// System status line
    status_line: String,
    /// Context window info
    context_info: (u32, u32, f32),
    /// Provider and model info
    provider_info: (String, String),
    /// Status blink state (for PROCESSING)
    status_blink: bool,
    /// Last status blink time
    last_status_blink: Instant,
    /// Whether we're in processing mode (for cursor display)
    is_processing: bool,
    /// Should exit
    should_exit: bool,
    /// Track the last tool header line index for updating it
    last_tool_header_index: Option<usize>,
    /// Token rate tracking for wave animation
    token_wave_history: VecDeque<f64>, // Wave animation values for tokens
    /// SSE rate tracking for wave animation
    sse_wave_history: VecDeque<f64>, // Wave animation values for SSEs
    /// Start time for token tracking
    _session_start: Instant,  // Prefixed with _ to indicate it's intentionally unused for now
    /// SSE counter (including pings)
    sse_count: u32,
    /// Last token count for rate calculation
    last_token_count: u32,
}

impl TerminalState {
    fn new(theme: ColorTheme) -> Self {
        Self {
            theme,
            input_buffer: String::new(),
            cursor_position: 0,
            output_history: vec![
                "WEYLAND-YUTANI SYSTEMS".to_string(),
                "MU/TH/UR 6000 - INTERFACE 2.4.1".to_string(),
                "".to_string(),
                "SYSTEM INITIALIZED".to_string(),
                "AWAITING COMMAND...".to_string(),
                "".to_string(),
            ],
            scroll_offset: 0,
            cursor_blink: true,
            activity_animation: 0.0,
            activity_animation_target: 0.0,
            tool_activity: Vec::new(),
            tool_activity_auto_scroll: true,
            tool_activity_scroll: 0,
            last_visible_height: 0, // Will be set on first draw
            manual_scroll: false,
            last_blink: Instant::now(),
            status_line: "READY".to_string(),
            context_info: (0, 0, 0.0),
            provider_info: ("UNKNOWN".to_string(), "UNKNOWN".to_string()),
            status_blink: true,
            last_status_blink: Instant::now(),
            is_processing: false,
            should_exit: false,
            last_tool_header_index: None,
            token_wave_history: VecDeque::with_capacity(40), // Keep 40 points for wave animation
            sse_wave_history: VecDeque::with_capacity(40), // Keep 40 points for wave animation
            _session_start: Instant::now(),
            last_token_count: 0,
            sse_count: 0,
        }
    }

    /// Format tool call output
    fn format_tool_output(&mut self, tool_name: &str, caption: &str, content: &str) {
        // Add tool header bar to main output
        let header_text = format!(" {} | {}", tool_name.to_uppercase(), caption);
        
        // Add marker for special styling
        self.output_history.push(format!("[TOOL_HEADER]{}", header_text));
        
        // Track the index of this tool header for later updates
        self.last_tool_header_index = Some(self.output_history.len() - 1);
        
        self.output_history.push(String::new()); // Empty line after header  
        
        // Add the actual tool content to the tool detail panel
        self.tool_activity.clear(); // Clear previous activity
        self.tool_activity.push(format!("[{}] {}", tool_name.to_uppercase(), caption));
        self.tool_activity.push(String::new());
        for line in content.lines() {
            self.tool_activity.push(line.to_string());
        }
        
        // Auto-scroll to bottom of tool activity if auto-scroll is enabled
        if self.tool_activity_auto_scroll {
            // Use the actual height of the tool detail area (8 lines total, minus 2 for borders = 6)
            let visible_height = 6;
            if self.tool_activity.len() > visible_height { 
                self.tool_activity_scroll = self.tool_activity.len().saturating_sub(visible_height);
            }
        }
        
        // Auto-scroll to bottom only if user hasn't manually scrolled
        if !self.manual_scroll {
            let total_lines = self.output_history.len();
            let visible_height = self.last_visible_height.max(1);
            
            // Calculate scroll to ensure ALL lines including the last are visible
            if total_lines > visible_height {
                // The problem: we want to show lines from scroll_offset to scroll_offset + visible_height - 1
                // To see the last line (at index total_lines - 1), we need:
                // scroll_offset + visible_height - 1 >= total_lines - 1
                // scroll_offset >= total_lines - visible_height
                // But we also need to ensure we're not cutting off content
                // So we add 1 to ensure the last line is fully visible
                self.scroll_offset = total_lines.saturating_sub(visible_height.saturating_sub(1));
            } else {
                self.scroll_offset = 0;
            }
        }
    }

    /// Update tool header with completion status and timing
    fn update_tool_completion(&mut self, name: &str, success: bool, duration_ms: u128, caption: &str) {
        // Find and update the last tool header in place
        if let Some(index) = self.last_tool_header_index {
            if index < self.output_history.len() {
                // Format the timing info
                let timing = if duration_ms < 1000 {
                    format!("{}ms", duration_ms)
                } else {
                    format!("{:.2}s", duration_ms as f64 / 1000.0)
                };
                
                // Create the updated header with status marker and timing
                let status_marker = if success { "[SUCCESS]" } else { "[FAILED]" };
                let header_text = format!(" {} | {} | {}", name.to_uppercase(), caption, timing);
                
                // Replace the existing header line with the updated one
                self.output_history[index] = format!("{}{}", status_marker, header_text);
                
                // Clear the tracking index
                self.last_tool_header_index = None;
            }
        }
    }

    /// Update tool detail panel without changing the header
    fn update_tool_detail(&mut self, name: &str, content: &str) {
        // Update the tool detail panel with the complete content
        self.tool_activity.clear();
        self.tool_activity.push(format!("[{}] Complete", name.to_uppercase()));
        self.tool_activity.push(String::new());
        
        // Add all the content lines
        for line in content.lines() {
            self.tool_activity.push(line.to_string());
        }
        
        // Auto-scroll to bottom of tool activity if auto-scroll is enabled
        if self.tool_activity_auto_scroll {
            let visible_height = 6; // Tool detail area is 8 lines minus 2 for borders
            if self.tool_activity.len() > visible_height {
                self.tool_activity_scroll = self.tool_activity.len().saturating_sub(visible_height);
            }
        }
    }

    /// Parse markdown and convert to styled lines
    fn parse_markdown_line(&self, line: &str) -> Line<'_> {
        // Skip parsing for special status lines to preserve their formatting
        if line.starts_with("[SUCCESS]") || 
           line.starts_with("[FAILED]") || 
           line.starts_with("[TOOL_HEADER]") {
            // These should be handled elsewhere, but as a safety check
            return Line::from(Span::styled(
                format!(" {}", line),
                Style::default().fg(self.theme.terminal_green.to_color()),
            ));
        }

        let mut spans = Vec::new();
        let mut chars = line.chars().peekable();
        let mut current_text = String::new();
        
        // Check for headers first
        if let Some(stripped) = line.strip_prefix("### ") {
            return Line::from(Span::styled(
                format!(" {}", stripped),
                Style::default()
                    .fg(self.theme.terminal_cyan.to_color())
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
        } else if let Some(stripped) = line.strip_prefix("## ") {
            return Line::from(Span::styled(
                format!(" {}", stripped),
                Style::default()
                    .fg(self.theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ));
        } else if let Some(stripped) = line.strip_prefix("# ") {
            return Line::from(Span::styled(
                format!(" {}", stripped),
                Style::default()
                    .fg(self.theme.terminal_green.to_color())
                    .add_modifier(Modifier::BOLD),
            ));
        }
        
        // Check for code block markers
        if line.starts_with("```") {
            return Line::from(Span::styled(
                format!(" {}", line),
                Style::default()
                    .fg(self.theme.terminal_dim_green.to_color())
                    .bg(Color::Rgb(40, 42, 54)), // Dark background for code blocks
            ));
        }
        
        // Add leading space
        spans.push(Span::raw(" "));
        
        // Parse inline formatting
        while let Some(ch) = chars.next() {
            if ch == '*' {
                // Check for bold (**) or italic (*)
                if chars.peek() == Some(&'*') {
                    chars.next(); // consume second *
                    // Save current text
                    if !current_text.is_empty() {
                        spans.push(Span::styled(
                            current_text.clone(),
                            Style::default().fg(self.theme.terminal_green.to_color()),
                        ));
                        current_text.clear();
                    }
                    // Find closing **
                    let mut bold_text = String::new();
                    while let Some(ch) = chars.next() {
                        if ch == '*' && chars.peek() == Some(&'*') {
                            chars.next(); // consume second *
                            break;
                        }
                        bold_text.push(ch);
                    }
                    spans.push(Span::styled(
                        bold_text,
                        Style::default()
                            .fg(self.theme.terminal_amber.to_color())
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    // Handle italic (*)
                    if !current_text.is_empty() {
                        spans.push(Span::styled(
                            current_text.clone(),
                            Style::default().fg(self.theme.terminal_green.to_color()),
                        ));
                        current_text.clear();
                    }
                    // Find closing *
                    let mut italic_text = String::new();
                    for ch in chars.by_ref() {
                        if ch == '*' {
                            break;
                        }
                        italic_text.push(ch);
                    }
                    spans.push(Span::styled(
                        italic_text,
                        Style::default()
                            .fg(self.theme.terminal_cyan.to_color())
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            } else if ch == '`' {
                // Handle inline code
                if !current_text.is_empty() {
                    spans.push(Span::styled(
                        current_text.clone(),
                        Style::default().fg(self.theme.terminal_green.to_color()),
                    ));
                    current_text.clear();
                }
                // Find closing `
                let mut code_text = String::new();
                for ch in chars.by_ref() {
                    if ch == '`' {
                        break;
                    }
                    code_text.push(ch);
                }
                spans.push(Span::styled(
                    code_text,
                    Style::default()
                        .fg(self.theme.terminal_cyan.to_color())
                        .bg(Color::Rgb(40, 42, 54)),
                ));
            } else {
                current_text.push(ch);
            }
        }
        
        // Add any remaining text
        if !current_text.is_empty() {
            spans.push(Span::styled(
                current_text,
                Style::default().fg(self.theme.terminal_green.to_color()),
            ));
        }
        
        // Return the line with all spans
        if spans.len() > 1 { // More than just the leading space
            Line::from(spans)
        } else {
            // Fallback to plain text if no formatting found
            Line::from(Span::styled(
                format!(" {}", line),
                Style::default().fg(self.theme.terminal_green.to_color()),
            ))
        }
    }

    /// Add text to output history
    fn add_output(&mut self, text: &str) {
        let mut lines = text.lines();

        // Remove any existing cursor from the last line before adding new content
        if let Some(last) = self.output_history.last_mut() {
            if last.ends_with('█') {
                last.pop();
            }
        }

        // Handle the first line specially
        if let Some(first_line) = lines.next() {
            if let Some(last) = self.output_history.last_mut() {
                // Append first fragment to the last element
                last.push_str(first_line);
            } else {
                // No existing elements, just push the first line
                self.output_history.push(first_line.to_string());
            }
        }

        // Push the remaining lines individually
        for line in lines {
            self.output_history.push(line.to_string());
        }

        // Always add cursor at the end if we're in PROCESSING mode
        if self.is_processing {
            if let Some(last) = self.output_history.last_mut() {
                // Add a solid cursor at the end of the last line
                last.push('█');
            }
        }

        // Update scroll state
        // Auto-scroll to bottom only if user hasn't manually scrolled
        if !self.manual_scroll {
            let total_lines = self.output_history.len();
            let visible_height = self.last_visible_height.max(1);
            
            // Calculate scroll to ensure ALL lines including the last are visible
            if total_lines > visible_height {
                // The problem: we want to show lines from scroll_offset to scroll_offset + visible_height - 1
                // To see the last line (at index total_lines - 1), we need:
                // scroll_offset + visible_height - 1 >= total_lines - 1
                // scroll_offset >= total_lines - visible_height
                // But we also need to ensure we're not cutting off content
                // So we add 1 to ensure the last line is fully visible
                self.scroll_offset = total_lines.saturating_sub(visible_height.saturating_sub(1));
            } else {
                self.scroll_offset = 0;
            }
        }
    }
}

/// Public interface for the retro terminal
#[derive(Clone)]
pub struct RetroTui {
    tx: mpsc::UnboundedSender<TuiMessage>,
    state: Arc<Mutex<TerminalState>>,
    terminal: Arc<Mutex<Terminal<CrosstermBackend<io::Stdout>>>>,
}

impl RetroTui {
    /// Create and start the retro terminal UI
    pub async fn start(theme: ColorTheme) -> Result<Self> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        // Create message channel
        let (tx, mut rx) = mpsc::unbounded_channel::<TuiMessage>();

        let state = Arc::new(Mutex::new(TerminalState::new(theme)));
        let terminal = Arc::new(Mutex::new(terminal));

        // Clone for the background task
        let state_clone = state.clone();
        let terminal_clone = terminal.clone();

        // Spawn background task to handle messages and redraw
        tokio::spawn(async move {
            let mut last_draw = Instant::now();

            loop {
                // Check for messages
                while let Ok(msg) = rx.try_recv() {
                    let mut state = state_clone.lock().unwrap();
                    match msg {
                        TuiMessage::AgentOutput(text) => {
                            state.add_output(&text);
                        }
                        TuiMessage::ToolOutput {
                            name,
                            caption,
                            content,
                        } => {
                            state.format_tool_output(&name, &caption, &content);
                        }
                        TuiMessage::ToolDetailUpdate {
                            name,
                            content,
                        } => {
                            state.update_tool_detail(&name, &content);
                        }
                        TuiMessage::ToolComplete {
                            name,
                            success,
                            duration_ms,
                            caption,
                        } => {
                            state.update_tool_completion(&name, success, duration_ms, &caption);
                        }
                        TuiMessage::SystemStatus(status) => {
                            let was_processing = state.status_line == "PROCESSING";
                            state.status_line = status;
                            state.is_processing = state.status_line == "PROCESSING";
                            // Set animation target based on processing state
                            state.activity_animation_target = if state.is_processing { 1.0 } else { 0.0 };
                            
                            // Clear input buffer when entering PROCESSING mode
                            if !was_processing && state.is_processing {
                                state.input_buffer.clear();
                                state.cursor_position = 0;
                            }
                            
                            // Remove cursor when exiting PROCESSING mode
                            if was_processing && !state.is_processing {
                                if let Some(last) = state.output_history.last_mut() {
                                    if last.ends_with('█') {
                                        last.pop();
                                    }
                                }
                                state.manual_scroll = false; // Reset manual scroll
                            } else if !was_processing && state.is_processing {
                                // Add cursor when entering PROCESSING mode
                                if let Some(last) = state.output_history.last_mut() {
                                    last.push('█');
                                }
                            }
                        }
                        TuiMessage::ContextUpdate {
                            used,
                            total,
                            percentage,
                        } => {
                            state.context_info = (used, total, percentage);
                            
                            // Update token wave animation
                            let tokens_since_last = used.saturating_sub(state.last_token_count) as f64;
                            
                            // Add a wave point based on token rate (normalized 0-1)
                            let wave_value = (tokens_since_last / 100.0).min(1.0); // Normalize to 0-1
                            state.token_wave_history.push_back(wave_value);
                            
                            // Keep only last 40 data points for smooth animation
                            while state.token_wave_history.len() > 40 {
                                state.token_wave_history.pop_front();
                            }
                            
                            state.last_token_count = used;
                        }
                        TuiMessage::SSEReceived => {
                            state.sse_count += 1;
                            
                            // Add a pulse to the SSE wave animation
                            state.sse_wave_history.push_back(1.0); // Full pulse for each SSE
                            
                            // Decay older values for smooth animation
                            for i in 0..state.sse_wave_history.len().saturating_sub(1) {
                                if let Some(val) = state.sse_wave_history.get_mut(i) {
                                    *val *= 0.85; // Decay factor
                                }
                            }
                            
                            while state.sse_wave_history.len() > 40 {
                                state.sse_wave_history.pop_front();
                            }
                        }
                        TuiMessage::Error(err) => {
                            state.add_output(&format!("ERROR: {}", err));
                        }
                        TuiMessage::Exit => {
                            state.should_exit = true;
                            break;
                        }
                    }
                }

                // Check if we should exit
                if state_clone.lock().unwrap().should_exit {
                    break;
                }

                // Update cursor blink
                {
                    let mut state = state_clone.lock().unwrap();
                    if state.last_blink.elapsed() > Duration::from_millis(500) {
                        state.cursor_blink = !state.cursor_blink;
                        state.last_blink = Instant::now();
                    }

                    // Update status blink only if status is "PROCESSING"
                    if state.status_line == "PROCESSING" && state.last_status_blink.elapsed() > Duration::from_millis(500) {
                        state.status_blink = !state.status_blink;
                        state.last_status_blink = Instant::now();
                    }
                    
                    // Update activity area animation
                    let animation_speed = 0.15; // Adjust for faster/slower animation
                    if (state.activity_animation - state.activity_animation_target).abs() > 0.01 {
                        // Smoothly interpolate towards target
                        state.activity_animation += (state.activity_animation_target - state.activity_animation) * animation_speed;
                        // Clamp to valid range
                        state.activity_animation = state.activity_animation.clamp(0.0, 1.0);
                    } else {
                        // Snap to target when close enough
                        state.activity_animation = state.activity_animation_target;
                    }
                }

                // Redraw at ~60fps
                if last_draw.elapsed() > Duration::from_millis(16) {
                    let mut state = state_clone.lock().unwrap();
                    let mut term = terminal_clone.lock().unwrap();
                    let _ = Self::draw(&mut term, &mut state);
                    last_draw = Instant::now();
                }

                // Small sleep to prevent busy waiting
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        // Initial draw
        {
            let mut state = state.lock().unwrap();
            let mut term = terminal.lock().unwrap();
            Self::draw(&mut term, &mut state)?;
        }

        Ok(Self {
            tx,
            state,
            terminal,
        })
    }

    /// Draw the terminal UI
    fn draw(
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        state: &mut TerminalState,
    ) -> Result<()> {
        terminal.draw(|f| {
            let size = f.area();
            
            // Calculate activity area height based on animation (0 to 8)
            let activity_height = (8.0 * state.activity_animation).round() as u16;
            
            // Create main layout - dynamically adjust based on whether activity area is shown
            let chunks = if activity_height > 0 {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(5), // Header/input area
                        Constraint::Min(10),   // Main output area (will be further split)
                        Constraint::Length(activity_height), // Activity area (animated)
                        Constraint::Length(1), // Status bar
                    ])
                    .split(size)
            } else {
                // When activity area is hidden, give more space to output
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(5), // Header/input area
                        Constraint::Min(10),   // Main output area gets all remaining space
                        Constraint::Length(1), // Status bar
                    ])
                    .split(size)
            };

            // IMPORTANT: Update the last known visible height BEFORE drawing
            // This ensures auto-scroll calculations use the correct height
            let old_height = state.last_visible_height;
            // Calculate the actual visible height accounting for padding (2 lines)
            let new_visible_height = chunks[1].height.saturating_sub(2) as usize;
            
            // Only update if we have a valid height
            if new_visible_height > 0 {
                state.last_visible_height = new_visible_height;
            }

            // If the height changed and we're auto-scrolling, recalculate scroll position
            if old_height != state.last_visible_height && !state.manual_scroll {
                let total_lines = state.output_history.len();
                if total_lines > state.last_visible_height {
                    // Recalculate to show the bottom content
                    state.scroll_offset = total_lines.saturating_sub(state.last_visible_height);
                }
            }
            
            // Draw header/input area
            Self::draw_input_area(f, chunks[0], &state.input_buffer, state.cursor_position, state.cursor_blink, state.is_processing, &state.theme);

            // Draw main output area
            Self::draw_output_area(f, chunks[1], state, &state.output_history, state.scroll_offset, &state.theme);
            
            // Draw activity area only if it's visible (during animation or when shown)
            if activity_height > 0 {
                // Apply fade effect by adjusting opacity through color intensity
                let opacity = state.activity_animation;
                Self::draw_activity_area(f, chunks[2], state, opacity, &state.theme);
            }

            // Draw status bar - use the last chunk which is either index 2 or 3
            let status_bar_chunk = if activity_height > 0 {
                chunks[3]
            } else {
                chunks[2]
            };
            Self::draw_status_bar(
                f,
                status_bar_chunk,
                &state.status_line,
                state.context_info,
                &state.provider_info,
                state.status_blink,
                &state.theme,
            );
        })?;

        Ok(())
    }

    /// Draw the input area with prompt
    fn draw_input_area(f: &mut Frame, area: Rect, input_buffer: &str, cursor_position: usize, cursor_blink: bool, is_processing: bool, theme: &ColorTheme) {
        let prompt = "g3> ";
        let prompt_len = prompt.len();
        
        // Calculate available width for text (accounting for borders and prompt)
        let available_width = area.width.saturating_sub(2).saturating_sub(prompt_len as u16) as usize;
        
        // Don't show cursor if processing
        let show_cursor = !is_processing && cursor_blink;
        
        // Build the display text with cursor at the right position
        let mut display_text = String::new();
        display_text.push_str(prompt);
        
        if input_buffer.is_empty() {
            // Empty buffer - just show cursor if applicable
            if show_cursor {
                display_text.push('█');
            }
        } else {
            // Calculate which part of the buffer to show (handle wrapping)
            let total_cursor_pos = cursor_position;
            
            // Determine the window into the buffer we should show
            let window_start = total_cursor_pos.saturating_sub(available_width - 1);
            
            // Get the visible portion of the buffer
            let visible_buffer: String = input_buffer
                .chars()
                .skip(window_start)
                .take(available_width)
                .collect();
            
            // Insert cursor at the appropriate position in the visible text
            let visible_cursor_pos = cursor_position.saturating_sub(window_start);
            
            for (i, ch) in visible_buffer.chars().enumerate() {
                if i == visible_cursor_pos && show_cursor {
                    display_text.push('█');
                    // Don't add the character under the cursor if we're showing the block cursor
                } else {
                    display_text.push(ch);
                }
            }
            
            // If cursor is at the end and we're showing it
            if visible_cursor_pos == visible_buffer.len() && show_cursor {
                display_text.push('█');
            }
        }

        let input = Paragraph::new(display_text)
            .style(Style::default().fg(theme.terminal_green.to_color()))
            .block(
                Block::default()
                    .title(" COMMAND INPUT ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.terminal_dim_green.to_color()))
                    .style(Style::default().bg(theme.terminal_bg.to_color())),
            );

        f.render_widget(input, area);
    }

    /// Draw the main output area
    fn draw_output_area(
        f: &mut Frame,
        area: Rect,
        state: &TerminalState,
        output_history: &[String],
        scroll_offset: usize,
        theme: &ColorTheme,
    ) {
        // Calculate visible lines (no borders now, but padding takes 2 lines)
        let visible_height = area.height.saturating_sub(2) as usize; // Account for padding
        let total_lines = output_history.len();

        // Calculate the proper scroll position
        let scroll = if total_lines <= visible_height {
            // If all content fits, no scrolling needed
            0
        } else {
            // Allow scrolling SCROLL_PAST_END_BUFFER lines past the normal end
            // This provides a buffer to ensure no content is cut off
            let max_scroll_with_buffer = total_lines.saturating_sub(visible_height).saturating_add(SCROLL_PAST_END_BUFFER);
            
            // If the requested scroll would show past the end, adjust it
            if scroll_offset > max_scroll_with_buffer {
                max_scroll_with_buffer
            } else {
                scroll_offset
            }
        };

        let mut in_code_block = false;

        // Get visible lines
        let visible_lines: Vec<Line> = output_history
            .iter()
            .skip(scroll)
            .take(visible_height)
            .map(|line| {
                // Check if this is a tool header line
                if line.starts_with("[TOOL_HEADER]") {
                    // Extract the actual header text
                    let cleaned = line.replace("[TOOL_HEADER]", "");
                    // Style with amber background and black text
                    return Line::from(Span::styled(
                        format!(" {}", cleaned),
                        Style::default()
                            .bg(theme.terminal_amber.to_color()) 
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if line.starts_with("[SUCCESS]") {
                    // Extract the actual header text
                    let cleaned = line.replace("[SUCCESS]", "");
                    // Style with green background for successful tool completion
                    return Line::from(Span::styled(
                        format!(" {}", cleaned),
                        Style::default()
                            .bg(theme.terminal_success.to_color())  // Use dedicated success color
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if line.starts_with("[FAILED]") {
                    // Extract the actual header text
                    let cleaned = line.replace("[FAILED]", "");
                    // Style with red background for failed tool completion
                    return Line::from(Span::styled(
                        format!(" {}", cleaned),
                        Style::default()
                            .bg(theme.terminal_red.to_color())
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ));
                }

                // Check for code block boundaries
                if line.starts_with("```") {
                    in_code_block = !in_code_block;
                }

                // If we're in a code block, style it appropriately
                if in_code_block && !line.starts_with("```") {
                    return Line::from(Span::styled(
                        format!(" {}", line),
                        Style::default()
                            .fg(theme.terminal_cyan.to_color())
                            .bg(Color::Rgb(40, 42, 54)),
                    ));
                }

                // Check if this is a box border line
                if line.starts_with("┌")
                    || line.starts_with("└")
                    || line.starts_with("│")
                    || line.starts_with("├")
                {
                    return Line::from(Span::styled(
                        format!(" {}", line),
                        Style::default().fg(theme.terminal_dim_green.to_color()),
                    ));
                }

                // Don't apply markdown parsing to tool status lines - preserve their original styling
                if line.starts_with("[SUCCESS]") || line.starts_with("[FAILED]") || line.starts_with("[TOOL_HEADER]") {
                    // These are already handled above, this shouldn't be reached
                    // but just in case, return the line as-is with appropriate color
                    return Line::from(Span::styled(
                        format!(" {}", line),
                        Style::default().fg(theme.terminal_green.to_color()),
                    ));
                }

                // Check if line contains markdown formatting
                if line.contains("**") || line.contains('`') || line.starts_with('#') {
                    // Use the markdown parser
                    return state.parse_markdown_line(line);
                }

                // Apply different colors based on content (existing logic)
                let style = if line.starts_with("ERROR:") {
                    Style::default()
                        .fg(theme.terminal_red.to_color())
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with('>') {
                    Style::default().fg(theme.terminal_cyan.to_color())
                } else if line.starts_with("SYSTEM:")
                    || line.starts_with("WEYLAND")
                    || line.starts_with("MU/TH/UR")
                {
                    Style::default()
                        .fg(theme.terminal_amber.to_color())
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with("SYSTEM INITIALIZED")
                    || line.starts_with("AWAITING COMMAND")
                {
                    Style::default()
                        .fg(theme.terminal_dim_green.to_color())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.terminal_green.to_color())
                };

                Line::from(Span::styled(format!(" {}", line), style))
            })
            .collect();

        let output = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    // Remove borders but keep the block for spacing
                    .borders(Borders::NONE)
                    // Add padding to maintain the same spacing as borders would provide
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 1))
                    .style(Style::default().bg(theme.terminal_bg.to_color())),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(output, area);

        // Draw scrollbar if needed
        if total_lines > visible_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .style(Style::default().fg(theme.terminal_dim_green.to_color()));

            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(scroll)
                .viewport_content_length(visible_height);

            f.render_stateful_widget(
                scrollbar,
                area.inner(ratatui::layout::Margin {
                    vertical: 0,  // No borders, so no vertical margin needed
                    horizontal: 0, // Keep horizontal margin at 0
                }),
                &mut scrollbar_state,
            );
        }
    }

    /// Draw the activity area with tool output
    fn draw_activity_area(
        f: &mut Frame,
        area: Rect,
        state: &TerminalState,
        opacity: f32,
        theme: &ColorTheme,
    ) {
        // Note: scroll_offset is managed by the state and auto-scrolls to show latest content when new data arrives
        
        // Apply fade effect by adjusting colors based on opacity
        let fade_color = |color: Color| -> Color {
            match color {
                Color::Rgb(r, g, b) => {
                    let faded_r = (r as f32 * opacity) as u8;
                    let faded_g = (g as f32 * opacity) as u8;
                    let faded_b = (b as f32 * opacity) as u8;
                    Color::Rgb(faded_r, faded_g, faded_b)
                }
                _ => color,
            }
        };
        
        // Split the activity area into left and right halves
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50), // Left half for tool output
                Constraint::Percentage(50), // Right half (reserved for future use)
            ])
            .split(area);
        
        // Draw left half - Tool Activity
        // Calculate actual visible height accounting for borders
        let visible_height = chunks[0].height.saturating_sub(2).max(1) as usize;
        let total_lines = state.tool_activity.len();
        let scroll_offset = state.tool_activity_scroll;
        // Calculate scroll position
        let scroll = if total_lines <= visible_height {
            0
        } else {
            scroll_offset.min(total_lines.saturating_sub(visible_height))
        };
        
        // Get visible lines for tool activity
        let visible_lines: Vec<Line> = if state.tool_activity.is_empty() {
            vec![Line::from(Span::styled(
                " No tool activity yet",
                Style::default().fg(fade_color(theme.terminal_dim_green.to_color())).add_modifier(Modifier::ITALIC),
            ))]
        } else {
            state.tool_activity
                .iter()
                .skip(scroll)
                .take(visible_height)
                .map(|line| {
                    // Style the header lines differently
                    let style = if line.starts_with('[') && line.contains(']') {
                        Style::default().fg(fade_color(theme.terminal_cyan.to_color())).add_modifier(Modifier::BOLD)
                    } else if line.is_empty() {
                        Style::default()
                    } else {
                        Style::default().fg(fade_color(theme.terminal_green.to_color()))
                    };
                    Line::from(Span::styled(format!(" {}", line), style))
                })
                .collect()
        };
        
        let tool_output = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .title(" TOOL DETAIL ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(fade_color(theme.terminal_dim_green.to_color())))
                    .style(Style::default().bg(theme.terminal_bg.to_color())),
            )
            .wrap(Wrap { trim: false });
        
        f.render_widget(tool_output, chunks[0]);
        
        // Draw right half - Activity graphs with wave animations
        Self::draw_activity_graphs(f, chunks[1], &state.token_wave_history, &state.sse_wave_history, opacity, theme);
    }
    
    /// Draw activity graphs with wave animations for tokens and SSEs
    fn draw_activity_graphs(
        f: &mut Frame,
        area: Rect,
        token_wave: &VecDeque<f64>,
        sse_wave: &VecDeque<f64>,
        opacity: f32,
        theme: &ColorTheme,
    ) {
        // Apply fade effect by adjusting colors based on opacity
        let fade_color = |color: Color| -> Color {
            match color {
                Color::Rgb(r, g, b) => {
                    let faded_r = (r as f32 * opacity) as u8;
                    let faded_g = (g as f32 * opacity) as u8;
                    let faded_b = (b as f32 * opacity) as u8;
                    Color::Rgb(faded_r, faded_g, faded_b)
                }
                _ => color,
            }
        };
        
        // Create the chart block
        let block = Block::default()
            .title(" ACTIVITY ")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(fade_color(theme.terminal_dim_green.to_color())))
            .style(Style::default().bg(theme.terminal_bg.to_color()));
        
        // Calculate inner area for chart
        let inner = block.inner(area);
        
        // Render the block first
        f.render_widget(block, area);
        
        // If area too small, don't render graphs
        if inner.width < 10 || inner.height < 4 {
            return;
        }
        
        // Split the inner area into two graphs (top and bottom)
        let graph_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(50), // Top graph for tokens
                Constraint::Percentage(50), // Bottom graph for SSEs
            ])
            .split(inner);
        
        // Draw token wave graph (top)
        Self::draw_wave_graph(
            f,
            graph_chunks[0],
            token_wave,
            "TOKENS",
            fade_color(theme.terminal_cyan.to_color()),
            fade_color(theme.terminal_dim_green.to_color()),
            opacity,
        );
        
        // Draw SSE wave graph (bottom)
        Self::draw_wave_graph(
            f,
            graph_chunks[1],
            sse_wave,
            "SSE",
            fade_color(theme.terminal_green.to_color()),
            fade_color(theme.terminal_dim_green.to_color()),
            opacity,
        );
    }
    
    /// Draw a single wave animation graph
    fn draw_wave_graph(
        f: &mut Frame,
        area: Rect,
        wave_data: &VecDeque<f64>,
        label: &str,
        wave_color: Color,
        _axis_color: Color,
        _opacity: f32,
    ) {
        let width = area.width as usize;
        let height = area.height as usize;
        
        if height < 2 || width < 5 {
            return;
        }
        
        // Wave characters for smooth animation
        let wave_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        
        // Build the wave line
        let mut wave_line = String::new();
        wave_line.push_str(&format!("{:<6}", label)); // Left-aligned label
        
        // Calculate how many data points to show
        let display_width = width.saturating_sub(6); // Account for label
        
        // Generate wave visualization
        for i in 0..display_width {
            let idx = wave_data.len().saturating_sub(display_width) + i;
            
            if idx < wave_data.len() {
                let value = wave_data[idx].clamp(0.0, 1.0);
                let char_idx = ((value * 7.0) as usize).min(7);
                wave_line.push(wave_chars[char_idx]);
            } else {
                wave_line.push(wave_chars[0]); // Baseline
            }
        }
        
        // Create the wave line with color
        let wave_paragraph = Paragraph::new(vec![
            Line::from(Span::styled(wave_line, Style::default().fg(wave_color))),
        ]);
        
        f.render_widget(wave_paragraph, area);
    }
    
    /// Draw the status bar
    fn draw_status_bar(
        f: &mut Frame,
        area: Rect,
        status_line: &str,
        context_info: (u32, u32, f32),
        provider_info: &(String, String),
        status_blink: bool,
        theme: &ColorTheme,
    ) {
        let (used, total, percentage) = context_info;

        // Create context meter
        let bar_width = 10;
        let filled = ((percentage / 100.0) * bar_width as f32) as usize;
        let meter = format!("[{}{}]", "█".repeat(filled), "░".repeat(bar_width - filled));

        let (_, model) = provider_info;

        // Determine status color based on status text
        let (status_color, status_text) = if status_line == "PROCESSING" {
            // Blink the PROCESSING status
            if status_blink {
                (theme.terminal_dark_amber.to_color(), status_line)
            } else {
                (theme.terminal_bg.to_color(), "         ") // Hide text by matching background
            }
        } else if status_line == "READY" {
            (theme.terminal_pale_blue.to_color(), status_line)
        } else {
            // Default to amber for other statuses
            (theme.terminal_amber.to_color(), status_line)
        };

        // Build the status line with different colored spans
        let status_spans = vec![
            Span::styled(
                " STATUS: ",
                Style::default()
                    .fg(theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                status_text,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " | CONTEXT: ",
                Style::default()
                    .fg(theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} {:.1}% ({}/{})", meter, percentage, used, total),
                Style::default()
                    .fg(theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " | ",
                Style::default()
                    .fg(theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", model),
                Style::default()
                    .fg(theme.terminal_amber.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        let status_line = Line::from(status_spans);

        let status = Paragraph::new(status_line)
            .style(Style::default().bg(theme.terminal_bg.to_color()))
            .alignment(Alignment::Left);

        f.render_widget(status, area);
    }

    /// Send output to the terminal
    pub fn output(&self, text: &str) {
        let _ = self.tx.send(TuiMessage::AgentOutput(text.to_string()));
    }

    /// Send tool output to the terminal
    pub fn tool_output(&self, name: &str, caption: &str, content: &str) {
        let _ = self.tx.send(TuiMessage::ToolOutput {
            name: name.to_string(),
            caption: caption.to_string(),
            content: content.to_string(),
        });
    }

    /// Update tool detail panel without changing the header
    pub fn update_tool_detail(&self, name: &str, content: &str) {
        let _ = self.tx.send(TuiMessage::ToolDetailUpdate {
            name: name.to_string(),
            content: content.to_string(),
        });
    }

    /// Send tool completion status to the terminal
    pub fn tool_complete(&self, name: &str, success: bool, duration_ms: u128, caption: &str) {
        let _ = self.tx.send(TuiMessage::ToolComplete {
            name: name.to_string(),
            success,
            duration_ms,
            caption: caption.to_string(),
        });
    }

    /// Update system status
    pub fn status(&self, status: &str) {
        let _ = self.tx.send(TuiMessage::SystemStatus(status.to_string()));
    }

    /// Update context window information
    pub fn update_context(&self, used: u32, total: u32, percentage: f32) {
        let _ = self.tx.send(TuiMessage::ContextUpdate {
            used,
            total,
            percentage,
        });
    }

    /// Update provider and model info
    pub fn update_provider_info(&self, provider: &str, model: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.provider_info = (provider.to_string(), model.to_string());
        }
    }

    /// Notify that an SSE was received (including pings)
    pub fn sse_received(&self) {
        let _ = self.tx.send(TuiMessage::SSEReceived);
    }

    /// Send error message
    pub fn error(&self, error: &str) {
        let _ = self.tx.send(TuiMessage::Error(error.to_string()));
    }

    /// Signal exit
    pub fn exit(&self) {
        let _ = self.tx.send(TuiMessage::Exit);
    }

    /// Update input buffer (for display)
    pub fn update_input(&self, input: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.input_buffer = input.to_string();
            // Keep cursor at end when updating the whole buffer
            state.cursor_position = input.len();
        }
    }

    /// Move cursor left
    pub fn cursor_left(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.cursor_position > 0 {
                state.cursor_position -= 1;
            }
        }
    }
    
    /// Move cursor right
    pub fn cursor_right(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.cursor_position < state.input_buffer.len() {
                state.cursor_position += 1;
            }
        }
    }
    
    /// Move cursor to beginning of line (Ctrl-A)
    pub fn cursor_home(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cursor_position = 0;
        }
    }
    
    /// Move cursor to end of line (Ctrl-E)
    pub fn cursor_end(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cursor_position = state.input_buffer.len();
        }
    }
    
    /// Delete word before cursor (Ctrl-W)
    pub fn delete_word(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.cursor_position > 0 {
                // Find the start of the word to delete
                let mut word_start = state.cursor_position;
                let chars: Vec<char> = state.input_buffer.chars().collect();
                
                // Skip trailing spaces
                while word_start > 0 && chars[word_start - 1].is_whitespace() {
                    word_start -= 1;
                }
                
                // Find word boundary
                while word_start > 0 && !chars[word_start - 1].is_whitespace() {
                    word_start -= 1;
                }
                
                // Remove the word
                let before = state.input_buffer.chars().take(word_start).collect::<String>();
                let after = state.input_buffer.chars().skip(state.cursor_position).collect::<String>();
                state.input_buffer = format!("{}{}", before, after);
                state.cursor_position = word_start;
            }
        }
    }
    
    /// Delete from cursor to end of line (Ctrl-K)
    pub fn delete_to_end(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.input_buffer = state.input_buffer.chars().take(state.cursor_position).collect();
        }
    }
    
    /// Get current input buffer and cursor position
    pub fn get_input_state(&self) -> (String, usize) {
        if let Ok(state) = self.state.lock() {
            (state.input_buffer.clone(), state.cursor_position)
        } else {
            (String::new(), 0)
        }
    }
    
    /// Insert character at cursor position
    pub fn insert_char(&self, ch: char) {
        if let Ok(mut state) = self.state.lock() {
            let before = state.input_buffer.chars().take(state.cursor_position).collect::<String>();
            let after = state.input_buffer.chars().skip(state.cursor_position).collect::<String>();
            state.input_buffer = format!("{}{}{}", before, ch, after);
            state.cursor_position += 1;
        }
    }
    
    /// Delete character at cursor position (Delete key)
    pub fn delete_char(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.cursor_position < state.input_buffer.len() {
                let before = state.input_buffer.chars().take(state.cursor_position).collect::<String>();
                let after = state.input_buffer.chars().skip(state.cursor_position + 1).collect::<String>();
                state.input_buffer = format!("{}{}", before, after);
            }
        }
    }
    
    /// Delete character before cursor (Backspace)
    pub fn backspace(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.cursor_position > 0 {
                let before = state.input_buffer.chars().take(state.cursor_position - 1).collect::<String>();
                let after = state.input_buffer.chars().skip(state.cursor_position).collect::<String>();
                state.input_buffer = format!("{}{}", before, after);
                state.cursor_position -= 1;
            }
        }
    }
    
    /// Handle scrolling
    pub fn scroll_up(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.scroll_offset > 0 {
                state.manual_scroll = true;
                state.scroll_offset -= 1;
            }
        }
    }

    pub fn scroll_down(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.manual_scroll = true;
            let total_lines = state.output_history.len();
            let visible_height = state.last_visible_height.max(1);

            // Calculate max scroll position
            // Allow scrolling SCROLL_PAST_END_BUFFER lines past what would normally be the end
            // This gives some buffer space at the bottom
            let max_scroll = total_lines.saturating_sub(visible_height).saturating_add(SCROLL_PAST_END_BUFFER);
            
            state.scroll_offset = (state.scroll_offset + 1).min(max_scroll);
        }
    }

    pub fn scroll_page_up(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.manual_scroll = true;
            // Use the last known visible height, or a reasonable default
            // The actual visible area is typically around 20-30 lines minus borders
            let page_size = if state.last_visible_height > 0 {
                state.last_visible_height.saturating_sub(2) // Leave a couple lines for context
            } else {
                15 // Reasonable default
            };

            if state.scroll_offset > 0 {
                // Scroll up by a page worth of lines
                state.scroll_offset = state.scroll_offset.saturating_sub(page_size);
            }
        }
    }

    pub fn scroll_page_down(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.manual_scroll = true;
            let total_lines = state.output_history.len();
            let visible_height = state.last_visible_height.max(1);
            
            let page_size = if state.last_visible_height > 0 {
                state.last_visible_height.saturating_sub(2) // Leave a couple lines for context
            } else {
                15 // Reasonable default
            };

            // Calculate max scroll position
            // Allow scrolling SCROLL_PAST_END_BUFFER lines past what would normally be the end
            let max_scroll = total_lines.saturating_sub(visible_height).saturating_add(SCROLL_PAST_END_BUFFER);

            // Scroll down by a page, but don't go past the end
            state.scroll_offset = (state.scroll_offset + page_size).min(max_scroll);
        }
    }

    pub fn scroll_home(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.scroll_offset = 0;
        }
    }

    pub fn scroll_end(&self) {
        if let Ok(mut state) = self.state.lock() {
            let total_lines = state.output_history.len();
            let visible_height = state.last_visible_height.max(1);
            
            // Scroll to show the last page of content plus SCROLL_PAST_END_BUFFER extra lines
            // This ensures we can see past the end a bit for safety
            state.scroll_offset = total_lines.saturating_sub(visible_height).saturating_add(SCROLL_PAST_END_BUFFER);
            
            // When scrolling to end, disable manual scroll so auto-scroll resumes
            state.manual_scroll = false;
        }
    }
}

impl Drop for RetroTui {
    fn drop(&mut self) {
        // Restore terminal
        let _ = disable_raw_mode();
        if let Ok(mut term) = self.terminal.lock() {
            let _ = execute!(
                term.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            );
        }
    }
}
