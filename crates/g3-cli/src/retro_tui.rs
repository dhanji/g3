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

// Retro sci-fi color scheme inspired by Alien terminals
const TERMINAL_GREEN: Color = Color::Rgb(136, 244, 152); // Mid green
const TERMINAL_AMBER: Color = Color::Rgb(242, 204, 148); // Softer amber for warnings
const TERMINAL_DIM_GREEN: Color = Color::Rgb(154, 174, 135); // softer vintage green for borders
const TERMINAL_BG: Color = Color::Rgb(0, 10, 0); // Very dark green background
const TERMINAL_CYAN: Color = Color::Rgb(0, 255, 255); // Cyan for highlights
const TERMINAL_RED: Color = Color::Rgb(239, 119, 109); // Red for errors or negative diffs
const TERMINAL_PALE_BLUE: Color = Color::Rgb(173, 234, 251); // Pale blue for READY status
const TERMINAL_DARK_AMBER: Color = Color::Rgb(204, 119, 34); // Dark amber for PROCESSING status
const TERMINAL_WHITE: Color = Color::Rgb(218, 218, 219); // Dimmer white for punchy text

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
    SystemStatus(String),
    ContextUpdate {
        used: u32,
        total: u32,
        percentage: f32,
    },
    Error(String),
    Exit,
}

/// Shared state for the retro terminal
struct TerminalState {
    /// Current input buffer
    input_buffer: String,
    /// Output history
    output_history: Vec<String>,
    /// Scroll position in output
    scroll_offset: usize,
    /// Cursor blink state
    cursor_blink: bool,
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
}

impl TerminalState {
    fn new() -> Self {
        Self {
            input_buffer: String::new(),
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
        }
    }

    /// Format tool call output with a box
    fn format_tool_output(&mut self, tool_name: &str, caption: &str, content: &str) {
        // Calculate box width (use a reasonable width, accounting for terminal size)
        let box_width = 80;
        let border_char = "─";
        let corner_tl = "┌";
        let corner_tr = "┐";
        let corner_bl = "└";
        let corner_br = "┘";
        let vertical = "│";

        // Add top border
        self.output_history.push(format!(
            "{}{}{}",
            corner_tl,
            border_char.repeat(box_width - 2),
            corner_tr
        ));

        // Add header with tool name (will be styled with green background in draw)
        let header_text = format!(" {} | {}", tool_name.to_uppercase(), caption);
        let padding = box_width - 2 - header_text.len();
        self.output_history.push(format!(
            "{}[TOOL_HEADER]{}{}{}",
            vertical,
            header_text,
            " ".repeat(padding),
            vertical
        ));

        // Add separator between header and content
        self.output_history.push(format!(
            "{}{}{}",
            "├",
            border_char.repeat(box_width - 2),
            "┤"
        ));

        // Add content lines
        for line in content.lines() {
            // Wrap long lines if needed
            let max_content_width = box_width - 4; // Account for borders and padding
            if line.len() <= max_content_width {
                self.output_history.push(format!(
                    "{} {:<width$} {}",
                    vertical,
                    line,
                    vertical,
                    width = max_content_width
                ));
            } else {
                // Simple word wrapping for long lines
                for chunk in line.chars().collect::<Vec<_>>().chunks(max_content_width) {
                    let chunk_str: String = chunk.iter().collect();
                    self.output_history.push(format!(
                        "{} {:<width$} {}",
                        vertical,
                        chunk_str,
                        vertical,
                        width = max_content_width
                    ));
                }
            }
        }

        // Add bottom border
        self.output_history.push(format!(
            "{}{}{}",
            corner_bl,
            border_char.repeat(box_width - 2),
            corner_br
        ));
        self.output_history.push(String::new()); // Empty line after box
        
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

    /// Add padding lines to ensure content can be scrolled fully into view
    fn add_padding(&mut self) {
        // Add enough blank lines to ensure the last content can be scrolled into view
        // This is a workaround for the scrolling calculation issues
        let padding_lines = 5; // Add 5 blank lines for padding
        for _ in 0..padding_lines {
            self.output_history.push(String::new());
        }
        // Reset scroll to show the actual content (not the padding)
        // This keeps the view focused on the last real content
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
    pub async fn start() -> Result<Self> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        // Create message channel
        let (tx, mut rx) = mpsc::unbounded_channel::<TuiMessage>();

        let state = Arc::new(Mutex::new(TerminalState::new()));
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
                        TuiMessage::SystemStatus(status) => {
                            let was_processing = state.status_line == "PROCESSING";
                            state.status_line = status;
                            state.is_processing = state.status_line == "PROCESSING";
                            
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
                    if state.status_line == "PROCESSING" {
                        if state.last_status_blink.elapsed() > Duration::from_millis(500) {
                            state.status_blink = !state.status_blink;
                            state.last_status_blink = Instant::now();
                        }
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
            
            // Create main layout - header, input, output
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5), // Header/input area
                    Constraint::Min(10),   // Main output area
                    Constraint::Length(1), // Status bar
                ])
                .split(size);

            // IMPORTANT: Update the last known visible height BEFORE drawing
            // This ensures auto-scroll calculations use the correct height
            let old_height = state.last_visible_height;
            // Calculate the actual visible height accounting for borders (2 lines)
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
            Self::draw_input_area(f, chunks[0], &state.input_buffer, state.cursor_blink);

            // Draw main output area
            Self::draw_output_area(f, chunks[1], &state.output_history, state.scroll_offset);

            // Draw status bar
            Self::draw_status_bar(
                f,
                chunks[2],
                &state.status_line,
                state.context_info,
                &state.provider_info,
                state.status_blink,
            );
        })?;

        Ok(())
    }

    /// Draw the input area with prompt
    fn draw_input_area(f: &mut Frame, area: Rect, input_buffer: &str, cursor_blink: bool) {
        // Show the actual input buffer content with prompt
        let input_text = if cursor_blink {
            format!("g3> {}█", input_buffer)
        } else {
            format!("g3> {} ", input_buffer)
        };

        let input = Paragraph::new(input_text)
            .style(Style::default().fg(TERMINAL_GREEN))
            .block(
                Block::default()
                    .title(" COMMAND INPUT ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(TERMINAL_DIM_GREEN))
                    .style(Style::default().bg(TERMINAL_BG)),
            );

        f.render_widget(input, area);
    }

    /// Draw the main output area
    fn draw_output_area(
        f: &mut Frame,
        area: Rect,
        output_history: &[String],
        scroll_offset: usize,
    ) {
        // Calculate visible lines
        let visible_height = area.height.saturating_sub(2) as usize; // Account for borders
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

        // Get visible lines
        let visible_lines: Vec<Line> = output_history
            .iter()
            .skip(scroll)
            .take(visible_height)
            .map(|line| {
                // Check if this is a tool header line
                if line.contains("[TOOL_HEADER]") {
                    // Extract the actual header text
                    let cleaned = line.replace("[TOOL_HEADER]", "");
                    // Style with green background and black text
                    return Line::from(Span::styled(
                        format!(" {}", cleaned),
                        Style::default()
                            .bg(TERMINAL_GREEN)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
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
                        Style::default().fg(TERMINAL_DIM_GREEN),
                    ));
                }
                // Apply different colors based on content
                let style = if line.starts_with("ERROR:") {
                    Style::default()
                        .fg(TERMINAL_RED)
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with('>') {
                    Style::default().fg(TERMINAL_CYAN)
                } else if line.starts_with("SYSTEM:")
                    || line.starts_with("WEYLAND")
                    || line.starts_with("MU/TH/UR")
                {
                    Style::default()
                        .fg(TERMINAL_AMBER)
                        .add_modifier(Modifier::BOLD)
                } else if line.starts_with("SYSTEM INITIALIZED")
                    || line.starts_with("AWAITING COMMAND")
                {
                    Style::default()
                        .fg(TERMINAL_DIM_GREEN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TERMINAL_GREEN)
                };

                Line::from(Span::styled(format!(" {}", line), style))
            })
            .collect();

        let output = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .title(" SYSTEM OUTPUT ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(TERMINAL_DIM_GREEN))
                    .style(Style::default().bg(TERMINAL_BG)),
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
                .style(Style::default().fg(TERMINAL_DIM_GREEN));

            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(scroll)
                .viewport_content_length(visible_height);

            f.render_stateful_widget(
                scrollbar,
                area.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut scrollbar_state,
            );
        }
    }

    /// Draw the status bar
    fn draw_status_bar(
        f: &mut Frame,
        area: Rect,
        status_line: &str,
        context_info: (u32, u32, f32),
        provider_info: &(String, String),
        status_blink: bool,
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
                (TERMINAL_DARK_AMBER, status_line)
            } else {
                (TERMINAL_BG, "         ") // Hide text by matching background
            }
        } else if status_line == "READY" {
            (TERMINAL_PALE_BLUE, status_line)
        } else {
            // Default to amber for other statuses
            (TERMINAL_AMBER, status_line)
        };

        // Build the status line with different colored spans
        let status_spans = vec![
            Span::styled(
                " STATUS: ",
                Style::default()
                    .fg(TERMINAL_AMBER)
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
                    .fg(TERMINAL_AMBER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} {:.1}% ({}/{})", meter, percentage, used, total),
                Style::default()
                    .fg(TERMINAL_AMBER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " | ",
                Style::default()
                    .fg(TERMINAL_AMBER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", model),
                Style::default()
                    .fg(TERMINAL_AMBER)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        let status_line = Line::from(status_spans);

        let status = Paragraph::new(status_line)
            .style(Style::default().bg(TERMINAL_BG))
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
