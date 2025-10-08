use anyhow::Result;
use clap::Parser;
use g3_config::Config;
use g3_core::{project::Project, ui_writer::UiWriter, Agent};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

mod retro_tui;
mod tui;
mod ui_writer_impl;
mod theme;
use retro_tui::RetroTui;
use tui::SimpleOutput;
use ui_writer_impl::{ConsoleUiWriter, RetroTuiWriter};
use theme::ColorTheme;

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

    // Load configuration
    let config = Config::load(cli.config.as_deref())?;

    // Initialize agent
    let ui_writer = ConsoleUiWriter::new();
    let mut agent = if cli.autonomous {
        Agent::new_autonomous(config.clone(), ui_writer).await?
    } else {
        Agent::new(config.clone(), ui_writer).await?
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
        output.print_markdown(&result);
    } else {
        // Interactive mode (default)
        if !cli.retro {
            info!("Starting interactive mode");
        }

        if cli.retro {
            // Use retro terminal UI
            run_interactive_retro(config, cli.show_prompt, cli.show_code, cli.theme).await?;
        } else {
            // Use standard terminal UI
            let output = SimpleOutput::new();
            output.print(&format!("📁 Workspace: {}", project.workspace().display()));
            run_interactive(agent, cli.show_prompt, cli.show_code).await?;
        }
    }

    Ok(())
}

async fn run_interactive_retro(config: Config, show_prompt: bool, show_code: bool, theme_name: Option<String>) -> Result<()> {
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
    let mut agent = Agent::new(config, ui_writer).await?;

    // Display initial system messages
    tui.output("SYSTEM: AGENT ONLINE\n\n");
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
                                Ok(response) => {
                                    tui.output(&response);
                                    tui.status("READY");
                                }
                                Err(e) => {
                                    tui.error(&format!("Task execution failed: {}", e));
                                    tui.status("ERROR");
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
) -> Result<()> {
    let output = SimpleOutput::new();

    output.print("");
    output.print("🤖 G3 AI Coding Agent - Interactive Mode");
    output.print(
        "I solve problems by writing and executing code. Tell me what you need to accomplish!",
    );
    output.print("");

    // Display provider and model information
    match agent.get_provider_info() {
        Ok((provider, model)) => {
            output.print(&format!("🔧 Provider: {} | Model: {}", provider, model));
        }
        Err(e) => {
            error!("Failed to get provider info: {}", e);
        }
    }

    output.print("");
    output.print("Type 'exit' or 'quit' to exit, use Up/Down arrows for command history");
    output.print("For multiline input: use \\ at the end of a line to continue");
    output.print("Submit multiline with Enter (without backslash)");
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
    // Show thinking indicator immediately
    output.print("🤔 Thinking...");
    // Note: flush is handled internally by println

    // Create cancellation token for this request
    let cancellation_token = CancellationToken::new();
    let cancel_token_clone = cancellation_token.clone();

    // Execute task with cancellation support
    let execution_result = tokio::select! {
        result = agent.execute_task_with_timing_cancellable(
            input, None, false, show_prompt, show_code, true, cancellation_token
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
        Ok(response) => output.print_markdown(&response),
        Err(e) => {
            if e.to_string().contains("cancelled") {
                output.print("⚠️  Operation cancelled by user");
            } else {
                // Enhanced error logging with detailed information
                error!("=== TASK EXECUTION ERROR ===");
                error!("Error: {}", e);

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
                if e.to_string().contains("No response received") {
                    output.print("💡 This may be a temporary issue. Please try again or check the logs for more details.");
                    output.print("   Log files are saved in the 'logs/' directory.");
                }
            }
        }
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
        return Ok(());
    }

    // Read requirements
    let requirements = match project.read_requirements()? {
        Some(content) => content,
        None => {
            output.print("❌ Error: Could not read requirements.md");
            return Ok(());
        }
    };

    output.print("📋 Requirements loaded from requirements.md");
    output.print("🔄 Starting coach-player feedback loop...");

    let mut turn = 1;
    let mut coach_feedback = String::new();
    let mut implementation_approved = false;

    loop {
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

        // Execute player task and handle the result properly
        match agent
            .execute_task_with_timing(&player_prompt, None, false, show_prompt, show_code, true)
            .await
        {
            Ok(player_result) => {
                // Display player's implementation result
                output.print("📝 Player implementation completed:");
                output.print_markdown(&player_result);
            }
            Err(e) => {
                output.print(&format!("❌ Player implementation failed: {}", e));
                // Continue to coach review even if player had an error
            }
        }

        // Give some time for file operations to complete
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Create a new agent instance for coach mode to ensure fresh context
        let config = g3_config::Config::load(None)?;
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
        let coach_result = coach_agent
            .execute_task_with_timing(&coach_prompt, None, false, show_prompt, show_code, true)
            .await?;

        output.print("🎓 Coach review completed");
        
        // Extract the actual feedback text from the coach result
        // IMPORTANT: We only want the final_output summary, not the entire conversation
        // The coach_result contains the full conversation including file reads, analysis, etc.
        // We need to extract ONLY the final_output content
        
        let coach_feedback_text = {
            // Look for the final_output content in the coach's response
            // In autonomous mode, the final_output is returned without the "=> " prefix
            // The coach result should end with the summary content from final_output
            
            // First, remove any timing information at the end
            let content_without_timing = if let Some(timing_pos) = coach_result.rfind("\n⏱️") {
                &coach_result[..timing_pos]
            } else {
                &coach_result
            };
            
            // The final_output content is typically the last substantial text in the response
            // after all tool executions. Look for it after the last tool execution marker
            // or take the last paragraph if no clear markers
            
            // Split by double newlines to find the last substantial block
            let blocks: Vec<&str> = content_without_timing.split("\n\n").collect();
            
            // Find the last non-empty block that isn't just whitespace
            let final_block = blocks.iter()
                .rev()
                .find(|block| !block.trim().is_empty())
                .map(|block| block.trim().to_string())
                .unwrap_or_else(|| {
                    // Fallback: if we can't find a clear block, take the whole thing
                    // but this shouldn't happen if the coach properly calls final_output
                    content_without_timing.trim().to_string()
                });
            
            final_block
        };
        
        // Log the size of the feedback for debugging
        info!(
            "Coach feedback extracted: {} characters (from {} total)",
            coach_feedback_text.len(),
            coach_result.len()
        );
        
        // Check if we got empty feedback (this can happen if the coach doesn't call final_output)
        if coach_feedback_text.is_empty() {
            output.print("⚠️ Coach did not provide feedback. This may be a model issue.");
            coach_feedback = "The implementation needs review. Please ensure all requirements are met and the code compiles without errors.".to_string();
            turn += 1;
            continue;
        }
        
        output.print(&format!("Coach feedback:\n{}", coach_feedback_text));

        // Check if coach approved the implementation
        if coach_feedback_text.contains("IMPLEMENTATION_APPROVED") {
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

    if implementation_approved {
        output.print("\n🎉 Autonomous mode completed successfully");
    } else {
        output.print("\n🔄 Autonomous mode completed (max iterations)");
    }

    Ok(())
}
