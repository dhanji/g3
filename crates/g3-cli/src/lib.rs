
/// Extract coach feedback by reading from the coach agent's specific log file
/// Uses the coach agent's session ID to find the exact log file
fn extract_coach_feedback_from_logs(_coach_result: &g3_core::TaskResult, coach_agent: &g3_core::Agent<ConsoleUiWriter>, output: &SimpleOutput) -> Result<String> {
    // CORRECT APPROACH: Get the session ID from the current coach agent
    // and read its specific log file directly
    
    // Get the coach agent's session ID
    let session_id = coach_agent.get_session_id()
        .ok_or_else(|| anyhow::anyhow!("Coach agent has no session ID"))?;
    
    // Construct the log file path for this specific coach session
    let logs_dir = std::path::Path::new("logs");
    let log_file_path = logs_dir.join(format!("g3_session_{}.json", session_id));
    
    // Read the coach agent's specific log file
    if log_file_path.exists() {
        if let Ok(log_content) = std::fs::read_to_string(&log_file_path) {
            if let Ok(log_json) = serde_json::from_str::<serde_json::Value>(&log_content) {
                if let Some(context_window) = log_json.get("context_window") {
                    if let Some(conversation_history) = context_window.get("conversation_history") {
                        if let Some(messages) = conversation_history.as_array() {
                            // Simply get the last message content - this is the coach's final feedback
                            if let Some(last_message) = messages.last() {
                                if let Some(content) = last_message.get("content") {
                                    if let Some(content_str) = content.as_str() {
                                        output.print(&format!("✅ Extracted coach feedback from session: {}", session_id));
                                        return Ok(content_str.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    Err(anyhow::anyhow!("Could not extract feedback from coach session: {}", session_id))
}use anyhow::Result;
use clap::Parser;
use g3_config::Config;
use g3_core::{project::Project, ui_writer::UiWriter, Agent};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::Path;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use g3_core::error_handling::{classify_error, ErrorType, RecoverableError};
mod retro_tui;
mod theme;
mod tui;
mod ui_writer_impl;
use retro_tui::RetroTui;
use theme::ColorTheme;
use tui::SimpleOutput;
use ui_writer_impl::{ConsoleUiWriter, RetroTuiWriter};

#[derive(Parser)]
#[command(name = "g3")]
#[command(about = "A modular, composable AI coding agent")]
#[command(version)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Show the system prompt being sent to the LLM
    #[arg(long)]
    pub show_prompt: bool,

    /// Show the generated code before execution
    #[arg(long)]
    pub show_code: bool,

    /// Configuration file path
    #[arg(short, long)]
    pub config: Option<String>,

    /// Workspace directory (defaults to current directory)
    #[arg(short, long)]
    pub workspace: Option<PathBuf>,

    /// Task to execute (if provided, runs in single-shot mode instead of interactive)
    pub task: Option<String>,

    /// Enable autonomous mode with coach-player feedback loop
    #[arg(long)]
    pub autonomous: bool,

    /// Maximum number of turns in autonomous mode (default: 5)
    #[arg(long, default_value = "5")]
    pub max_turns: usize,

    /// Use retro terminal UI (inspired by 80s sci-fi)
    #[arg(long)]
    pub retro: bool,

    /// Color theme for retro mode (default, dracula, or path to theme file)
    #[arg(long, value_name = "THEME")]
    pub theme: Option<String>,

    /// Override the configured provider (anthropic, databricks, embedded, openai)
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Override the model for the selected provider
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Only initialize logging if not in retro mode
    if !cli.retro {
        // Initialize logging with filtering
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

        // Create a filter that suppresses llama_cpp logs unless in verbose mode
        let filter = if cli.verbose {
            EnvFilter::from_default_env()
                .add_directive(format!("{}=debug", env!("CARGO_PKG_NAME")).parse().unwrap())
                .add_directive("g3_core=debug".parse().unwrap())
                .add_directive("g3_cli=debug".parse().unwrap())
                .add_directive("g3_execution=debug".parse().unwrap())
                .add_directive("g3_providers=debug".parse().unwrap())
        } else {
            EnvFilter::from_default_env()
                .add_directive(format!("{}=info", env!("CARGO_PKG_NAME")).parse().unwrap())
                .add_directive("g3_core=info".parse().unwrap())
                .add_directive("g3_cli=info".parse().unwrap())
                .add_directive("g3_execution=info".parse().unwrap())
                .add_directive("g3_providers=info".parse().unwrap())
                .add_directive("llama_cpp=off".parse().unwrap()) // Suppress all llama_cpp logs
                .add_directive("llama=off".parse().unwrap()) // Suppress all llama.cpp logs
        };

        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(filter)
            .init();
    } else {
        // In retro mode, we don't want any logging output to interfere with the TUI
        // We'll use a no-op subscriber
        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

        // Create a filter that suppresses ALL logs in retro mode
        let filter = EnvFilter::from_default_env().add_directive("off".parse().unwrap()); // Turn off all logging

        tracing_subscriber::registry().with(filter).init();
    }

    if !cli.retro {
        info!("Starting G3 AI Coding Agent");
    }

    // Set up workspace directory
    let workspace_dir = if let Some(ws) = cli.workspace {
        ws
    } else if cli.autonomous {
        // For autonomous mode, use G3_WORKSPACE env var or default
        setup_workspace_directory()?
    } else {
        // Default to current directory for interactive/single-shot mode
        std::env::current_dir()?
    };

    // Check if we're in a project directory and read README if available
    // This should happen in both interactive and autonomous modes
    let readme_content = read_project_readme(&workspace_dir);

    // Create project model
    let project = if cli.autonomous {
        Project::new_autonomous(workspace_dir.clone())?
    } else {
        Project::new(workspace_dir.clone())
    };

    // Ensure workspace exists and enter it
    project.ensure_workspace_exists()?;
    project.enter_workspace()?;

    if !cli.retro {
        info!("Using workspace: {}", project.workspace().display());
    }

    // Load configuration with CLI overrides
    let config = Config::load_with_overrides(
        cli.config.as_deref(),
        cli.provider.clone(),
        cli.model.clone(),
    )?;
    
    // Validate provider if specified
    if let Some(ref provider) = cli.provider {
        let valid_providers = ["anthropic", "databricks", "embedded", "openai"];
        if !valid_providers.contains(&provider.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid provider '{}'. Valid options: {:?}", 
                provider, valid_providers
            ));
        }
    }

    // Initialize agent
    let ui_writer = ConsoleUiWriter::new();
    let mut agent = if cli.autonomous {
        Agent::new_autonomous_with_readme(config.clone(), ui_writer, readme_content.clone()).await?
    } else {
        Agent::new_with_readme(config.clone(), ui_writer, readme_content.clone()).await?
    };

    // Execute task, autonomous mode, or start interactive mode
    if cli.autonomous {
        // Autonomous mode with coach-player feedback loop
        if !cli.retro {
            info!("Starting autonomous mode");
        }
        run_autonomous(
            agent,
            project,
            cli.show_prompt,
            cli.show_code,
            cli.max_turns,
        )
        .await?;
    } else if let Some(task) = cli.task {
        // Single-shot mode
        if !cli.retro {
            info!("Executing task: {}", task);
        }
        let output = SimpleOutput::new();
        let result = agent
            .execute_task_with_timing(&task, None, false, cli.show_prompt, cli.show_code, true)
            .await?;
        output.print_smart(&result.response);
    } else {
        // Interactive mode (default)
        if !cli.retro {
            info!("Starting interactive mode");
        }

        if cli.retro {
            // Use retro terminal UI
            run_interactive_retro(
                config,  // Already has overrides applied
                cli.show_prompt,
                cli.show_code,
                cli.theme,
                readme_content,
            )
            .await?;
        } else {
            // Use standard terminal UI
            let output = SimpleOutput::new();
            output.print(&format!("📁 Workspace: {}", project.workspace().display()));
            run_interactive(agent, cli.show_prompt, cli.show_code, readme_content).await?;
        }
    }

    Ok(())
}

/// Check if we're in a project directory and read README if available
fn read_project_readme(workspace_dir: &Path) -> Option<String> {
    // Check if we're in a project directory (contains .g3 or .git)
    let is_project_dir = workspace_dir.join(".g3").exists() || workspace_dir.join(".git").exists();

    if !is_project_dir {
        return None;
    }

    // Look for README files in common formats
    let readme_names = [
        "README.md",
        "README.MD",
        "readme.md",
        "Readme.md",
        "README",
        "README.txt",
        "README.rst",
    ];

    for readme_name in &readme_names {
        let readme_path = workspace_dir.join(readme_name);
        if readme_path.exists() {
            match std::fs::read_to_string(&readme_path) {
                Ok(content) => {
                    // Return the content with a note about which file was read
                    return Some(format!(
                        "📚 Project README (from {}):\n\n{}",
                        readme_name, content
                    ));
                }
                Err(e) => {
                    // Log the error but continue looking for other README files
                    error!("Failed to read {}: {}", readme_path.display(), e);
                }
            }
        }
    }

    None
}

/// Extract the main heading or title from README content
fn extract_readme_heading(readme_content: &str) -> Option<String> {
    // Process the content line by line, skipping the prefix line if present
    let lines_iter = readme_content.lines();
    let mut content_lines = Vec::new();

    for line in lines_iter {
        // Skip the "📚 Project README (from ...):" line
        if line.starts_with("📚 Project README") {
            continue;
        }
        content_lines.push(line);
    }
    let content = content_lines.join("\n");

    // Look for the first markdown heading
    for line in content.lines() {
        let trimmed = line.trim();

        // Check for H1 heading (# Title)
        if trimmed.starts_with("# ") {
            let title = trimmed[2..].trim();
            if !title.is_empty() {
                // Return the full title (including any description after dash)
                return Some(title.to_string());
            }
        }

        // Skip other markdown headings for now (##, ###, etc.)
        // We're only looking for the main H1 heading
    }

    // If no H1 heading found, look for the first non-empty, non-metadata line as a fallback
    for line in content.lines().take(5) {
        let trimmed = line.trim();
        // Skip empty lines, other heading markers, and metadata
        if !trimmed.is_empty()
            && !trimmed.starts_with("📚")
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("==")
            && !trimmed.starts_with("--")
        {
            // Limit length for display
            return Some(if trimmed.len() > 100 {
                format!("{}...", &trimmed[..97])
            } else {
                trimmed.to_string()
            });
        }
    }
    None
}

async fn run_interactive_retro(
    config: Config,
    show_prompt: bool,
    show_code: bool,
    theme_name: Option<String>,
    readme_content: Option<String>,
) -> Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use std::time::Duration;

    // Set environment variable to suppress println in other crates
    std::env::set_var("G3_RETRO_MODE", "1");

    // Load the color theme
    let theme = match ColorTheme::load(theme_name.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to load theme: {}. Using default.", e);
            ColorTheme::default()
        }
    };

    // Initialize the retro terminal UI
    let tui = RetroTui::start(theme).await?;

    // Create agent with RetroTuiWriter
    let ui_writer = RetroTuiWriter::new(tui.clone());
    let mut agent = Agent::new_with_readme(config, ui_writer, readme_content.clone()).await?;

    // Display initial system messages
    tui.output("SYSTEM: AGENT ONLINE\n\n");

    // Display message if README was loaded
    if readme_content.is_some() {
        // Extract the first heading or title from the README
        let readme_snippet = if let Some(ref content) = readme_content {
            extract_readme_heading(content)
                .unwrap_or_else(|| "PROJECT DOCUMENTATION LOADED".to_string())
        } else {
            "PROJECT DOCUMENTATION LOADED".to_string()
        };
        tui.output(&format!(
            "SYSTEM: PROJECT README LOADED - {}\n\n",
            readme_snippet
        ));
    }
    tui.output("SYSTEM: READY FOR INPUT\n\n");
    tui.output("\n\n");

    // Display provider and model information
    match agent.get_provider_info() {
        Ok((provider, model)) => {
            tui.update_provider_info(&provider, &model);
        }
        Err(e) => {
            tui.update_provider_info("ERROR", &e.to_string());
        }
    }

    // Track multiline input
    let mut multiline_buffer = String::new();
    let mut in_multiline = false;

    // Main event loop
    loop {
        // Update context window display
        let context = agent.get_context_window();
        tui.update_context(
            context.used_tokens,
            context.total_tokens,
            context.percentage_used(),
        );

        // Poll for keyboard events
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.exit();
                        break;
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.exit();
                        break;
                    }
                    // Emacs/bash-like shortcuts
                    KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.cursor_home();
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.cursor_end();
                    }
                    KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.delete_word();
                    }
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.delete_to_end();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Delete from beginning to cursor (similar to Ctrl-K but opposite direction)
                        let (input_buffer, cursor_pos) = tui.get_input_state();
                        if cursor_pos > 0 {
                            let after = input_buffer.chars().skip(cursor_pos).collect::<String>();
                            tui.update_input(&after);
                            tui.cursor_home();
                        }
                    }
                    KeyCode::Left => {
                        tui.cursor_left();
                    }
                    KeyCode::Right => {
                        tui.cursor_right();
                    }
                    KeyCode::Home if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.cursor_home();
                    }
                    KeyCode::End if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.cursor_end();
                    }
                    KeyCode::Delete => {
                        tui.delete_char();
                    }
                    KeyCode::Enter => {
                        let (input_buffer, _) = tui.get_input_state();
                        if !input_buffer.is_empty() {
                            // Clear the input for next command
                            tui.update_input("");
                            let trimmed = input_buffer.trim_end();

                            // Check if line ends with backslash for continuation
                            if trimmed.ends_with('\\') {
                                // Remove the backslash and add to buffer
                                let without_backslash = &trimmed[..trimmed.len() - 1];
                                multiline_buffer.push_str(without_backslash);
                                multiline_buffer.push('\n');
                                in_multiline = true;
                                tui.status("MULTILINE INPUT");
                                continue;
                            }

                            // If we're in multiline mode and no backslash, this is the final line
                            let final_input = if in_multiline {
                                multiline_buffer.push_str(&input_buffer);
                                in_multiline = false;
                                let result = multiline_buffer.clone();
                                multiline_buffer.clear();
                                tui.status("READY");
                                result
                            } else {
                                input_buffer.clone()
                            };

                            let input = final_input.trim().to_string();
                            if input.is_empty() {
                                continue;
                            }

                            if input == "exit" || input == "quit" {
                                tui.exit();
                                break;
                            }

                            // Execute the task
                            tui.output(&format!("> {}", input));
                            tui.status("PROCESSING");

                            const MAX_TIMEOUT_RETRIES: u32 = 3;
                            let mut attempt = 0;

                            loop {
                                attempt += 1;

                                match agent
                                    .execute_task_with_timing(
                                        &input,
                                        None,
                                        false,
                                        show_prompt,
                                        show_code,
                                        true,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        if attempt > 1 {
                                            tui.output(&format!(
                                                "SYSTEM: REQUEST SUCCEEDED AFTER {} ATTEMPTS",
                                                attempt
                                            ));
                                        }
                                        tui.output(&result.response);
                                        tui.status("READY");
                                        break;
                                    }
                                    Err(e) => {
                                        // Check if this is a timeout error that we should retry
                                        let error_type = classify_error(&e);

                                        if matches!(
                                            error_type,
                                            ErrorType::Recoverable(RecoverableError::Timeout)
                                        ) && attempt < MAX_TIMEOUT_RETRIES
                                        {
                                            // Calculate retry delay with exponential backoff
                                            let delay_ms = 1000 * (2_u64.pow(attempt - 1));
                                            let delay = std::time::Duration::from_millis(delay_ms);

                                            tui.output(&format!("SYSTEM: TIMEOUT ERROR (ATTEMPT {}/{}). RETRYING IN {:?}...",
                                                attempt, MAX_TIMEOUT_RETRIES, delay));
                                            tui.status("RETRYING");

                                            // Wait before retrying
                                            tokio::time::sleep(delay).await;
                                            continue;
                                        }

                                        // For non-timeout errors or after max retries
                                        tui.error(&format!("Task execution failed: {}", e));
                                        tui.status("ERROR");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Char(c) => {
                        tui.insert_char(c);
                    }
                    KeyCode::Backspace => {
                        tui.backspace();
                    }
                    KeyCode::Up => {
                        tui.scroll_up();
                    }
                    KeyCode::Down => {
                        tui.scroll_down();
                    }
                    KeyCode::PageUp => {
                        tui.scroll_page_up();
                    }
                    KeyCode::PageDown => {
                        tui.scroll_page_down();
                    }
                    KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.scroll_home(); // Ctrl+Home for scrolling to top
                    }
                    KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tui.scroll_end(); // Ctrl+End for scrolling to bottom
                    }
                    _ => {}
                }
            }
        }

        // Small delay to prevent CPU spinning
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    tui.output("SYSTEM: SHUTDOWN INITIATED");
    Ok(())
}

async fn run_interactive<W: UiWriter>(
    mut agent: Agent<W>,
    show_prompt: bool,
    show_code: bool,
    readme_content: Option<String>,
) -> Result<()> {
    let output = SimpleOutput::new();

    output.print("");
    output.print("🪿 G3 AI Coding Agent");
    output.print("      >> what shall we build today?");
    output.print("");

    // Display provider and model information
    match agent.get_provider_info() {
        Ok((provider, model)) => {
            output.print(&format!("🔧 {} | {}", provider, model));
        }
        Err(e) => {
            error!("Failed to get provider info: {}", e);
        }
    }

    // Display message if README was loaded
    if readme_content.is_some() {
        // Extract the first heading or title from the README
        let readme_snippet = if let Some(ref content) = readme_content {
            extract_readme_heading(content)
                .unwrap_or_else(|| "Project documentation loaded".to_string())
        } else {
            "Project documentation loaded".to_string()
        };

        output.print(&format!("📚 detected: {}", readme_snippet));
    }

    output.print("");

    // Initialize rustyline editor with history
    let mut rl = DefaultEditor::new()?;

    // Try to load history from a file in the user's home directory
    let history_file = dirs::home_dir().map(|mut path| {
        path.push(".g3_history");
        path
    });

    if let Some(ref history_path) = history_file {
        let _ = rl.load_history(history_path);
    }

    // Track multiline input
    let mut multiline_buffer = String::new();
    let mut in_multiline = false;

    loop {
        // Display context window progress bar before each prompt
        display_context_progress(&agent, &output);

        // Adjust prompt based on whether we're in multi-line mode
        let prompt = if in_multiline { "... > " } else { "g3> " };

        let readline = rl.readline(prompt);
        match readline {
            Ok(line) => {
                let trimmed = line.trim_end();

                // Check if line ends with backslash for continuation
                if trimmed.ends_with('\\') {
                    // Remove the backslash and add to buffer
                    let without_backslash = &trimmed[..trimmed.len() - 1];
                    multiline_buffer.push_str(without_backslash);
                    multiline_buffer.push('\n');
                    in_multiline = true;
                    continue;
                }

                // If we're in multiline mode and no backslash, this is the final line
                if in_multiline {
                    multiline_buffer.push_str(&line);
                    in_multiline = false;
                    // Process the complete multiline input
                    let input = multiline_buffer.trim().to_string();
                    multiline_buffer.clear();

                    if input.is_empty() {
                        continue;
                    }

                    // Add complete multiline to history
                    rl.add_history_entry(&input)?;

                    if input == "exit" || input == "quit" {
                        break;
                    }

                    // Process the multiline input
                    execute_task(&mut agent, &input, show_prompt, show_code, &output).await;
                } else {
                    // Single line input
                    let input = line.trim().to_string();

                    if input.is_empty() {
                        continue;
                    }

                    if input == "exit" || input == "quit" {
                        break;
                    }

                    // Add to history
                    rl.add_history_entry(&input)?;

                    // Process the single line input
                    execute_task(&mut agent, &input, show_prompt, show_code, &output).await;
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C pressed
                if in_multiline {
                    // Cancel multiline input
                    output.print("Multi-line input cancelled");
                    multiline_buffer.clear();
                    in_multiline = false;
                } else {
                    output.print("CTRL-C");
                }
                continue;
            }
            Err(ReadlineError::Eof) => {
                output.print("CTRL-D");
                break;
            }
            Err(err) => {
                error!("Error: {:?}", err);
                break;
            }
        }
    }

    // Save history before exiting
    if let Some(ref history_path) = history_file {
        let _ = rl.save_history(history_path);
    }

    output.print("👋 Goodbye!");
    Ok(())
}

async fn execute_task<W: UiWriter>(
    agent: &mut Agent<W>,
    input: &str,
    show_prompt: bool,
    show_code: bool,
    output: &SimpleOutput,
) {
    const MAX_TIMEOUT_RETRIES: u32 = 3;
    let mut attempt = 0;
    // Show thinking indicator immediately
    output.print("🤔 Thinking...");
    // Note: flush is handled internally by println

    // Create cancellation token for this request
    let cancellation_token = CancellationToken::new();
    let cancel_token_clone = cancellation_token.clone();

    loop {
        attempt += 1;

        // Execute task with cancellation support
        let execution_result = tokio::select! {
            result = agent.execute_task_with_timing_cancellable(
                input, None, false, show_prompt, show_code, true, cancellation_token.clone()
            ) => {
                result
            }
            _ = tokio::signal::ctrl_c() => {
                cancel_token_clone.cancel();
                output.print("\n⚠️  Operation cancelled by user (Ctrl+C)");
                return;
            }
        };

        match execution_result {
            Ok(result) => {
                if attempt > 1 {
                    output.print(&format!("✅ Request succeeded after {} attempts", attempt));
                }
                output.print_smart(&result.response);
                return;
            }
            Err(e) => {
                if e.to_string().contains("cancelled") {
                    output.print("⚠️  Operation cancelled by user");
                    return;
                }

                // Check if this is a timeout error that we should retry
                let error_type = classify_error(&e);

                if matches!(
                    error_type,
                    ErrorType::Recoverable(RecoverableError::Timeout)
                ) && attempt < MAX_TIMEOUT_RETRIES
                {
                    // Calculate retry delay with exponential backoff
                    let delay_ms = 1000 * (2_u64.pow(attempt - 1));
                    let delay = std::time::Duration::from_millis(delay_ms);

                    output.print(&format!(
                        "⏱️  Timeout error detected (attempt {}/{}). Retrying in {:?}...",
                        attempt, MAX_TIMEOUT_RETRIES, delay
                    ));

                    // Wait before retrying
                    tokio::time::sleep(delay).await;
                    continue;
                }

                // For non-timeout errors or after max retries, handle as before
                handle_execution_error(&e, input, output, attempt);
                return;
            }
        }
    }
}

fn handle_execution_error(e: &anyhow::Error, input: &str, output: &SimpleOutput, attempt: u32) {
    // Enhanced error logging with detailed information
    error!("=== TASK EXECUTION ERROR ===");
    error!("Error: {}", e);
    if attempt > 1 {
        error!("Failed after {} attempts", attempt);
    }

    // Log error chain
    let mut source = e.source();
    let mut depth = 1;
    while let Some(err) = source {
        error!("  Caused by [{}]: {}", depth, err);
        source = err.source();
        depth += 1;
    }

    // Log additional context
    error!("Task input: {}", input);
    error!("Error type: {}", std::any::type_name_of_val(&e));

    // Display user-friendly error message
    output.print(&format!("❌ Error: {}", e));

    // If it's a stream error, provide helpful guidance
    if e.to_string().contains("No response received") || e.to_string().contains("timed out") {
        output.print("💡 This may be a temporary issue. Please try again or check the logs for more details.");
        output.print("   Log files are saved in the 'logs/' directory.");
    }
}

fn display_context_progress<W: UiWriter>(agent: &Agent<W>, output: &SimpleOutput) {
    let context = agent.get_context_window();
    output.print_context(
        context.used_tokens,
        context.total_tokens,
        context.percentage_used(),
    );
}

/// Set up the workspace directory for autonomous mode
/// Uses G3_WORKSPACE environment variable or defaults to ~/tmp/workspace
fn setup_workspace_directory() -> Result<PathBuf> {
    let workspace_dir = if let Ok(env_workspace) = std::env::var("G3_WORKSPACE") {
        PathBuf::from(env_workspace)
    } else {
        // Default to ~/tmp/workspace
        let home_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        home_dir.join("tmp").join("workspace")
    };

    // Create the directory if it doesn't exist
    if !workspace_dir.exists() {
        std::fs::create_dir_all(&workspace_dir)?;
        let output = SimpleOutput::new();
        output.print(&format!(
            "📁 Created workspace directory: {}",
            workspace_dir.display()
        ));
    }

    Ok(workspace_dir)
}

// Simplified autonomous mode implementation
async fn run_autonomous(
    mut agent: Agent<ConsoleUiWriter>,
    project: Project,
    show_prompt: bool,
    show_code: bool,
    max_turns: usize,
) -> Result<()> {
    let start_time = std::time::Instant::now();
    let output = SimpleOutput::new();

    output.print("🤖 G3 AI Coding Agent - Autonomous Mode");
    output.print(&format!(
        "📁 Using workspace: {}",
        project.workspace().display()
    ));

    // Check if requirements exist
    if !project.has_requirements() {
        output.print("❌ Error: requirements.md not found in workspace directory");
        output.print("   Please create a requirements.md file with your project requirements at:");
        output.print(&format!(
            "   {}/requirements.md",
            project.workspace().display()
        ));

        // Generate final report even for early exit
        let elapsed = start_time.elapsed();
        let context_window = agent.get_context_window();

        output.print(&format!("\n{}", "=".repeat(60)));
        output.print("📊 AUTONOMOUS MODE SESSION REPORT");
        output.print(&"=".repeat(60));

        output.print(&format!(
            "⏱️  Total Duration: {:.2}s",
            elapsed.as_secs_f64()
        ));
        output.print(&format!("🔄 Turns Taken: 0/{}", max_turns));
        output.print(&format!("📝 Final Status: ⚠️ NO REQUIREMENTS FILE"));

        output.print("\n📈 Token Usage Statistics:");
        output.print(&format!("   • Used Tokens: {}", context_window.used_tokens));
        output.print(&format!(
            "   • Total Available: {}",
            context_window.total_tokens
        ));
        output.print(&format!(
            "   • Cumulative Tokens: {}",
            context_window.cumulative_tokens
        ));
        output.print(&format!(
            "   • Usage Percentage: {:.1}%",
            context_window.percentage_used()
        ));
        output.print(&"=".repeat(60));

        return Ok(());
    }

    // Read requirements
    let requirements = match project.read_requirements()? {
        Some(content) => content,
        None => {
            output.print("❌ Error: Could not read requirements.md");

            // Generate final report even for early exit
            let elapsed = start_time.elapsed();
            let context_window = agent.get_context_window();

            output.print(&format!("\n{}", "=".repeat(60)));
            output.print("📊 AUTONOMOUS MODE SESSION REPORT");
            output.print(&"=".repeat(60));

            output.print(&format!(
                "⏱️  Total Duration: {:.2}s",
                elapsed.as_secs_f64()
            ));
            output.print(&format!("🔄 Turns Taken: 0/{}", max_turns));
            output.print(&format!("📝 Final Status: ⚠️ CANNOT READ REQUIREMENTS"));

            output.print("\n📈 Token Usage Statistics:");
            output.print(&format!("   • Used Tokens: {}", context_window.used_tokens));
            output.print(&format!(
                "   • Total Available: {}",
                context_window.total_tokens
            ));
            output.print(&format!(
                "   • Cumulative Tokens: {}",
                context_window.cumulative_tokens
            ));
            output.print(&format!(
                "   • Usage Percentage: {:.1}%",
                context_window.percentage_used()
            ));
            output.print(&"=".repeat(60));

            return Ok(());
        }
    };

    output.print("📋 Requirements loaded from requirements.md");
    output.print("🔄 Starting coach-player feedback loop...");

    // Check if implementation files already exist
    let skip_first_player = project.has_implementation_files();
    if skip_first_player {
        output.print("📂 Detected existing implementation files in workspace");
        output.print("⏭️  Skipping first player turn - proceeding directly to coach review");
    } else {
        output.print("📂 No existing implementation files detected");
        output.print("🎯 Starting with player implementation");
    }

    let mut turn = 1;
    let mut coach_feedback = String::new();
    let mut implementation_approved = false;

    loop {
        // Skip player turn if it's the first turn and implementation files exist
        if !(turn == 1 && skip_first_player) {
            output.print(&format!(
                "\n=== TURN {}/{} - PLAYER MODE ===",
                turn, max_turns
            ));

            // Player mode: implement requirements (with coach feedback if available)
            let player_prompt = if coach_feedback.is_empty() {
                format!(
                    "You are G3 in implementation mode. Read and implement the following requirements:\n\n{}\n\nImplement this step by step, creating all necessary files and code.",
                    requirements
                )
            } else {
                format!(
                    "You are G3 in implementation mode. Address the following specific feedback from the coach:\n\n{}\n\nContext: You are improving an implementation based on these requirements:\n{}\n\nFocus on fixing the issues mentioned in the coach feedback above.",
                    coach_feedback, requirements
                )
            };

            output.print("🎯 Starting player implementation...");

            // Display what feedback the player is receiving
            // If there's no coach feedback on subsequent turns, this is an error
            if coach_feedback.is_empty() {
                if turn > 1 {
                    return Err(anyhow::anyhow!("Player mode error: No coach feedback received on turn {}", turn));
                }
                output.print("📋 Player starting initial implementation (no prior coach feedback)");
            } else {
                output.print(&format!("📋 Player received coach feedback ({} chars):", coach_feedback.len()));
                output.print(&format!("{}", coach_feedback));
            }
            output.print(""); // Empty line for readability

            // Execute player task with retry on error
            let mut player_retry_count = 0;
            const MAX_PLAYER_RETRIES: u32 = 3;
            let mut player_failed = false;

            loop {
                match agent
                    .execute_task_with_timing(
                        &player_prompt,
                        None,
                        false,
                        show_prompt,
                        show_code,
                        true,
                    )
                    .await
                {
                    Ok(result) => {
                        // Display player's implementation result
                        output.print("📝 Player implementation completed:");
                        output.print_smart(&result.response);
                        break;
                    }
                    Err(e) => {
                        // Check if this is a panic (unrecoverable)
                        if e.to_string().contains("panic") {
                            output.print(&format!("💥 Player panic detected: {}", e));

                            // Generate final report even for panic
                            let elapsed = start_time.elapsed();
                            let context_window = agent.get_context_window();

                            output.print(&format!("\n{}", "=".repeat(60)));
                            output.print("📊 AUTONOMOUS MODE SESSION REPORT");
                            output.print(&"=".repeat(60));

                            output.print(&format!(
                                "⏱️  Total Duration: {:.2}s",
                                elapsed.as_secs_f64()
                            ));
                            output.print(&format!("🔄 Turns Taken: {}/{}", turn, max_turns));
                            output.print(&format!("📝 Final Status: 💥 PLAYER PANIC"));

                            output.print("\n📈 Token Usage Statistics:");
                            output.print(&format!(
                                "   • Used Tokens: {}",
                                context_window.used_tokens
                            ));
                            output.print(&format!(
                                "   • Total Available: {}",
                                context_window.total_tokens
                            ));
                            output.print(&format!(
                                "   • Cumulative Tokens: {}",
                                context_window.cumulative_tokens
                            ));
                            output.print(&format!(
                                "   • Usage Percentage: {:.1}%",
                                context_window.percentage_used()
                            ));
                            output.print(&"=".repeat(60));

                            return Err(e);
                        }

                        player_retry_count += 1;
                        output.print(&format!(
                            "⚠️ Player error (attempt {}/{}): {}",
                            player_retry_count, MAX_PLAYER_RETRIES, e
                        ));

                        if player_retry_count >= MAX_PLAYER_RETRIES {
                            output.print(
                                "🔄 Max retries reached for player, marking turn as failed...",
                            );
                            player_failed = true;
                            break; // Exit retry loop
                        }
                        output.print("🔄 Retrying player implementation...");
                    }
                }
            }

            // If player failed after max retries, increment turn and continue
            if player_failed {
                output.print(&format!(
                    "⚠️ Player turn {} failed after max retries. Moving to next turn.",
                    turn
                ));
                turn += 1;

                // Check if we've reached max turns
                if turn > max_turns {
                    output.print("\n=== SESSION COMPLETED - MAX TURNS REACHED ===");
                    output.print(&format!("⏰ Maximum turns ({}) reached", max_turns));
                    break;
                }

                // Continue to next iteration with empty feedback (restart from scratch)
                coach_feedback = String::new();
                continue;
            }

            // Give some time for file operations to complete
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        // Create a new agent instance for coach mode to ensure fresh context
        // Use the same config with overrides that was passed to the player agent
        let config = agent.get_config().clone();
        let ui_writer = ConsoleUiWriter::new();
        let mut coach_agent = Agent::new_autonomous(config, ui_writer).await?;

        // Ensure coach agent is also in the workspace directory
        project.enter_workspace()?;

        output.print(&format!(
            "\n=== TURN {}/{} - COACH MODE ===",
            turn, max_turns
        ));

        // Coach mode: critique the implementation
        let coach_prompt = format!(
            "You are G3 in coach mode. Your role is to critique and review implementations against requirements and provide concise, actionable feedback.

REQUIREMENTS:
{}

IMPLEMENTATION REVIEW:
Review the current state of the project and provide a concise critique focusing on:
1. Whether the requirements are correctly implemented
2. Whether the project compiles successfully
3. What requirements are missing or incorrect
4. Specific improvements needed to satisfy requirements

CRITICAL INSTRUCTIONS:
1. You MUST use the final_output tool to provide your feedback
2. The summary in final_output should be CONCISE and ACTIONABLE
3. Focus ONLY on what needs to be fixed or improved
4. Do NOT include your analysis process, file contents, or compilation output in the summary

If the implementation correctly meets all requirements and compiles without errors:
- Call final_output with summary: 'IMPLEMENTATION_APPROVED'

If improvements are needed:
- Call final_output with a brief summary listing ONLY the specific issues to fix

Remember: Be thorough in your review but concise in your feedback. APPROVE if the implementation works and generally fits the requirements.",
            requirements
        );

        output.print("🎓 Starting coach review...");

        // Execute coach task with retry on error
        let mut coach_retry_count = 0;
        const MAX_COACH_RETRIES: u32 = 3;
        let mut coach_failed = false;
        let coach_result_opt;

        loop {
            match coach_agent
                .execute_task_with_timing(&coach_prompt, None, false, show_prompt, show_code, true)
                .await
            {
                Ok(result) => {
                    coach_result_opt = Some(result);
                    break;
                }
                Err(e) => {
                    // Check if this is a panic (unrecoverable)
                    if e.to_string().contains("panic") {
                        output.print(&format!("💥 Coach panic detected: {}", e));

                        // Generate final report even for panic
                        let elapsed = start_time.elapsed();
                        let context_window = agent.get_context_window();

                        output.print(&format!("\n{}", "=".repeat(60)));
                        output.print("📊 AUTONOMOUS MODE SESSION REPORT");
                        output.print(&"=".repeat(60));

                        output.print(&format!(
                            "⏱️  Total Duration: {:.2}s",
                            elapsed.as_secs_f64()
                        ));
                        output.print(&format!("🔄 Turns Taken: {}/{}", turn, max_turns));
                        output.print(&format!("📝 Final Status: 💥 COACH PANIC"));

                        output.print("\n📈 Token Usage Statistics:");
                        output.print(&format!("   • Used Tokens: {}", context_window.used_tokens));
                        output.print(&format!(
                            "   • Total Available: {}",
                            context_window.total_tokens
                        ));
                        output.print(&format!(
                            "   • Cumulative Tokens: {}",
                            context_window.cumulative_tokens
                        ));
                        output.print(&format!(
                            "   • Usage Percentage: {:.1}%",
                            context_window.percentage_used()
                        ));
                        output.print(&"=".repeat(60));

                        return Err(e);
                    }

                    coach_retry_count += 1;
                    output.print(&format!(
                        "⚠️ Coach error (attempt {}/{}): {}",
                        coach_retry_count, MAX_COACH_RETRIES, e
                    ));

                    if coach_retry_count >= MAX_COACH_RETRIES {
                        output.print("🔄 Max retries reached for coach, using default feedback...");
                        // Provide default feedback and break out of retry loop
                        coach_result_opt = None;
                        coach_failed = true;
                        break; // Exit retry loop with default feedback
                    }
                    output.print("🔄 Retrying coach review...");
                }
            }
        }

        output.print("🎓 Coach review completed");

        // If coach failed after max retries, increment turn and continue with default feedback
        if coach_failed {
            output.print(&format!(
                "⚠️ Coach turn {} failed after max retries. Using default feedback.",
                turn
            ));
            coach_feedback = "The implementation needs review. Please ensure all requirements are met and the code compiles without errors.".to_string();
            turn += 1;

            if turn > max_turns {
                output.print("\n=== SESSION COMPLETED - MAX TURNS REACHED ===");
                output.print(&format!("⏰ Maximum turns ({}) reached", max_turns));
                break;
            }
            continue; // Continue to next iteration with default feedback
        }

        // We have a valid coach result, process it
        let coach_result = coach_result_opt.unwrap();

        // Extract the complete coach feedback from final_output
        let coach_feedback_text = extract_coach_feedback_from_logs(&coach_result, &coach_agent, &output)?;

        // Log the size of the feedback for debugging
        info!(
            "Coach feedback extracted: {} characters (from {} total)",
            coach_feedback_text.len(),
            coach_result.response.len()
        );

        // Check if we got empty feedback (this can happen if the coach doesn't call final_output)
        if coach_feedback_text.is_empty() {
            output.print("⚠️ Coach did not provide feedback. This may be a model issue.");
            coach_feedback = "The implementation needs review. Please ensure all requirements are met and the code compiles without errors.".to_string();
            turn += 1;
            continue;
        }

        output.print_smart(&format!("Coach feedback:\n{}", coach_feedback_text));

        // Check if coach approved the implementation
        if coach_result.is_approved() {
            output.print("\n=== SESSION COMPLETED - IMPLEMENTATION APPROVED ===");
            output.print("✅ Coach approved the implementation!");
            implementation_approved = true;
            break;
        }

        // Check if we've reached max turns
        if turn >= max_turns {
            output.print("\n=== SESSION COMPLETED - MAX TURNS REACHED ===");
            output.print(&format!("⏰ Maximum turns ({}) reached", max_turns));
            break;
        }

        // Store coach feedback for next iteration
        coach_feedback = coach_feedback_text;
        turn += 1;

        output.print("🔄 Coach provided feedback for next iteration");
    }

    // Generate final report
    let elapsed = start_time.elapsed();
    let context_window = agent.get_context_window();

    output.print(&format!("\n{}", "=".repeat(60)));
    output.print("📊 AUTONOMOUS MODE SESSION REPORT");
    output.print(&"=".repeat(60));

    output.print(&format!(
        "⏱️  Total Duration: {:.2}s",
        elapsed.as_secs_f64()
    ));
    output.print(&format!("🔄 Turns Taken: {}/{}", turn, max_turns));
    output.print(&format!(
        "📝 Final Status: {}",
        if implementation_approved {
            "✅ APPROVED"
        } else if turn >= max_turns {
            "⏰ MAX TURNS REACHED"
        } else {
            "⚠️ INCOMPLETE"
        }
    ));

    output.print("\n📈 Token Usage Statistics:");
    output.print(&format!("   • Used Tokens: {}", context_window.used_tokens));
    output.print(&format!(
        "   • Total Available: {}",
        context_window.total_tokens
    ));
    output.print(&format!(
        "   • Cumulative Tokens: {}",
        context_window.cumulative_tokens
    ));
    output.print(&format!(
        "   • Usage Percentage: {:.1}%",
        context_window.percentage_used()
    ));
    output.print(&"=".repeat(60));

    if implementation_approved {
        output.print("\n🎉 Autonomous mode completed successfully");
    } else {
        output.print("\n🔄 Autonomous mode terminated (max iterations)");
    }

    Ok(())
}
