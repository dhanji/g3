use anyhow::Result;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct TurnMetrics {
    turn_number: usize,
    tokens_used: u32,
    wall_clock_time: Duration,
}

/// Generate a histogram showing tokens used and wall clock time per turn
fn generate_turn_histogram(turn_metrics: &[TurnMetrics]) -> String {
    if turn_metrics.is_empty() {
        return "   No turn data available".to_string();
    }

    let mut histogram = String::new();
    
    // Find max values for scaling
    let max_tokens = turn_metrics.iter().map(|t| t.tokens_used).max().unwrap_or(1);
    let max_time_ms = turn_metrics.iter()
        .map(|t| t.wall_clock_time.as_millis().min(u32::MAX as u128) as u32)
        .max()
        .unwrap_or(1);
    
    // Constants for histogram display
    const MAX_BAR_WIDTH: usize = 40;
    const TOKEN_CHAR: char = '█';
    const TIME_CHAR: char = '▓';
    
    histogram.push_str("\n📊 Per-Turn Performance Histogram:\n");
    histogram.push_str(&format!("   {} = Tokens Used (max: {})\n", TOKEN_CHAR, max_tokens));
    histogram.push_str(&format!("   {} = Wall Clock Time (max: {:.1}s)\n\n", TIME_CHAR, max_time_ms as f64 / 1000.0));
    
    for metrics in turn_metrics {
        let turn_time_ms = metrics.wall_clock_time.as_millis().min(u32::MAX as u128) as u32;
        
        // Calculate bar lengths (proportional to max values)
        let token_bar_len = if max_tokens > 0 {
            ((metrics.tokens_used as f64 / max_tokens as f64) * MAX_BAR_WIDTH as f64) as usize
        } else {
            0
        };
        
        let time_bar_len = if max_time_ms > 0 {
            ((turn_time_ms as f64 / max_time_ms as f64) * MAX_BAR_WIDTH as f64) as usize
        } else {
            0
        };
        
        // Format time duration
        let time_str = if turn_time_ms < 1000 {
            format!("{}ms", turn_time_ms)
        } else if turn_time_ms < 60_000 {
            format!("{:.1}s", turn_time_ms as f64 / 1000.0)
        } else {
            let minutes = turn_time_ms / 60_000;
            let seconds = (turn_time_ms % 60_000) as f64 / 1000.0;
            format!("{}m{:.1}s", minutes, seconds)
        };
        
        // Create the bars
        let token_bar = TOKEN_CHAR.to_string().repeat(token_bar_len);
        let time_bar = TIME_CHAR.to_string().repeat(time_bar_len);
        
        // Add turn information
        histogram.push_str(&format!(
            "   Turn {:2}: {:>6} tokens │{:<40}│\n",
            metrics.turn_number,
            metrics.tokens_used,
            token_bar
        ));
        histogram.push_str(&format!(
            "           {:>6}       │{:<40}│\n",
            time_str,
            time_bar
        ));
        
        // Add separator line between turns (except for last turn)
        if metrics.turn_number != turn_metrics.last().unwrap().turn_number {
            histogram.push_str("           ────────────┼────────────────────────────────────────┤\n");
        }
    }
    
    // Add summary statistics
    let total_tokens: u32 = turn_metrics.iter().map(|t| t.tokens_used).sum();
    let total_time: Duration = turn_metrics.iter().map(|t| t.wall_clock_time).sum();
    let avg_tokens = total_tokens as f64 / turn_metrics.len() as f64;
    let avg_time_ms = total_time.as_millis() as f64 / turn_metrics.len() as f64;
    
    histogram.push_str("\n📈 Summary Statistics:\n");
    histogram.push_str(&format!("   • Total Tokens: {} across {} turns\n", total_tokens, turn_metrics.len()));
    histogram.push_str(&format!("   • Average Tokens/Turn: {:.1}\n", avg_tokens));
    histogram.push_str(&format!("   • Total Time: {:.1}s\n", total_time.as_secs_f64()));
    histogram.push_str(&format!("   • Average Time/Turn: {:.1}s\n", avg_time_ms / 1000.0));
    
    histogram
}

/// Extract coach feedback by reading from the coach agent's specific log file
/// Uses the coach agent's session ID to find the exact log file
fn extract_coach_feedback_from_logs(
    coach_result: &g3_core::TaskResult,
    coach_agent: &g3_core::Agent<ConsoleUiWriter>,
    output: &SimpleOutput,
) -> Result<String> {
    // CORRECT APPROACH: Get the session ID from the current coach agent
    // and read its specific log file directly

    // Get the coach agent's session ID
    let session_id = coach_agent
        .get_session_id()
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
                                        output.print(&format!(
                                            "✅ Extracted coach feedback from session: {}",
                                            session_id
                                        ));
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

    // If we couldn't extract from logs, panic with detailed error
    panic!(
        "CRITICAL: Could not extract coach feedback from session: {}\n\
         Log file path: {:?}\n\
         Log file exists: {}\n\
         This indicates the coach did not call any tool or the log is corrupted.\n\
         Coach result response length: {} chars",
        session_id,
        log_file_path,
        log_file_path.exists(),
        coach_result.response.len()
    );
}

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
mod ui_writer_impl;
mod simple_output;
use simple_output::SimpleOutput;
mod machine_ui_writer;
use machine_ui_writer::MachineUiWriter;
use ui_writer_impl::ConsoleUiWriter;

#[derive(Parser, Clone)]
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

    /// Override requirements text for autonomous mode (instead of reading from requirements.md)
    #[arg(long, value_name = "TEXT")]
    pub requirements: Option<String>,

    /// Interactive mode: prompt for requirements and save to requirements.md before starting autonomous mode
    #[arg(long)]
    pub interactive_requirements: bool,

    /// Enable machine-friendly output mode with JSON markers and stats
    #[arg(long)]
    pub machine: bool,

    /// Override the configured provider (anthropic, databricks, embedded, openai)
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Override the model for the selected provider
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Disable log file creation (no logs/ directory or session logs)
    #[arg(long)]
    pub quiet: bool,

    /// Enable macOS Accessibility API tools for native app automation
    #[arg(long)]
    pub macax: bool,

    /// Enable WebDriver browser automation tools
    #[arg(long)]
    pub webdriver: bool,

    /// Disable accumulative mode and use traditional interactive mode instead
    #[arg(long, help = "Disable accumulative mode (use traditional interactive chat)")]
    pub accumulative: bool,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    // Only initialize logging if not in retro mode
    if !cli.machine {
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

    if !cli.machine {
        info!("Starting G3 AI Coding Agent");
    }

    // Set up workspace directory
    let workspace_dir = if let Some(ws) = &cli.workspace {
        ws.clone()
    } else if cli.autonomous {
        // For autonomous mode, use G3_WORKSPACE env var or default
        setup_workspace_directory(cli.machine)?
    } else {
        // Default to current directory for interactive/single-shot mode
        std::env::current_dir()?
    };

    // Check if we're in a project directory and read README and AGENTS.md if available
    // Load AGENTS.md first (if present) to provide agent-specific instructions
    let agents_content = read_agents_config(&workspace_dir);
    
    // Then load README for project context
    let readme_content = read_project_readme(&workspace_dir);

    // Create project model
    let project = if cli.autonomous {
        // Handle interactive requirements mode with AI enhancement
        if cli.interactive_requirements {
            println!("\n📝 Interactive Requirements Mode");
            println!("================================\n");
            println!("Describe what you want to build (can be brief):");
            println!("Press Ctrl+D (Unix) or Ctrl+Z (Windows) when done.\n");
            
            use std::io::{self, Read, Write};
            let mut requirements_input = String::new();
            io::stdin().read_to_string(&mut requirements_input)?;
            
            if requirements_input.trim().is_empty() {
                anyhow::bail!("No requirements provided. Exiting.");
            }
            
            println!("\n🤖 Enhancing your requirements with AI...\n");
            
            // Create a temporary agent to enhance the requirements
            let temp_config = Config::load_with_overrides(
                cli.config.as_deref(),
                cli.provider.clone(),
                cli.model.clone(),
            )?;
            
            let ui_writer = ConsoleUiWriter::new();
            let mut temp_agent = Agent::new_with_readme_and_quiet(
                temp_config,
                ui_writer,
                None,
                true, // quiet mode
            ).await?;
            
            // Craft the enhancement prompt
            let enhancement_prompt = format!(
                r#"You are a requirements analyst. Take this brief user input and expand it into a structured requirements document.

USER INPUT:
{}

Create a professional requirements document with:
1. A clear project title (# heading)
2. An overview section explaining what will be built
3. Organized requirements (functional, technical, quality)
4. Acceptance criteria
5. Any technical constraints or preferences mentioned

Format as proper markdown. Be specific and actionable. If the user's input is vague, make reasonable assumptions but keep it focused on what they described.

Output ONLY the markdown content, no explanations or meta-commentary."#,
                requirements_input.trim()
            );
            
            // Execute enhancement task
            let result = temp_agent
                .execute_task_with_timing(&enhancement_prompt, None, false, false, false, false)
                .await?;
            
            let enhanced_requirements = result.response.trim().to_string();
            
            // Show the enhanced requirements
            println!("\n📋 Enhanced Requirements Document:");
            println!("{}\n", "=".repeat(60));
            println!("{}", enhanced_requirements);
            println!("{}\n", "=".repeat(60));
            
            // Ask for confirmation
            println!("\n❓ Is this requirements document acceptable?");
            println!("   [y] Yes, proceed with autonomous mode");
            println!("   [e] Edit and save manually");
            println!("   [n] No, cancel\n");
            
            print!("Your choice (y/e/n): ");
            io::stdout().flush()?;
            
            let mut choice = String::new();
            io::stdin().read_line(&mut choice)?;
            let choice = choice.trim().to_lowercase();
            
            let requirements_path = workspace_dir.join("requirements.md");
            
            match choice.as_str() {
                "y" | "yes" => {
                    // Save enhanced requirements
                    std::fs::write(&requirements_path, &enhanced_requirements)?;
                    println!("\n✅ Requirements saved to: {}", requirements_path.display());
                    println!("🚀 Starting autonomous mode...\n");
                }
                "e" | "edit" => {
                    // Save enhanced requirements for manual editing
                    std::fs::write(&requirements_path, &enhanced_requirements)?;
                    println!("\n✅ Requirements saved to: {}", requirements_path.display());
                    println!("📝 Please edit the file and run: g3 --autonomous");
                    println!("   Exiting for now.\n");
                    return Ok(());
                }
                "n" | "no" => {
                    println!("\n❌ Cancelled. No files were saved.\n");
                    return Ok(());
                }
                _ => {
                    println!("\n❌ Invalid choice. Cancelled.\n");
                    return Ok(());
                }
            }
        }
        
        if let Some(requirements_text) = &cli.requirements {
            // Use requirements text override
            Project::new_autonomous_with_requirements(workspace_dir.clone(), requirements_text.clone())?
        } else {
            // Use traditional requirements.md file
            Project::new_autonomous(workspace_dir.clone())?
        }
    } else {
        Project::new(workspace_dir.clone())
    };

    // Ensure workspace exists and enter it
    project.ensure_workspace_exists()?;
    project.enter_workspace()?;

    if !cli.machine {
        info!("Using workspace: {}", project.workspace().display());
    }

    // Load configuration with CLI overrides
    let mut config = Config::load_with_overrides(
        cli.config.as_deref(),
        cli.provider.clone(),
        cli.model.clone(),
    )?;

    // Apply macax flag override
    if cli.macax {
        config.macax.enabled = true;
        if !cli.machine {
            info!("macOS Accessibility API tools enabled");
        }
    }

    // Apply webdriver flag override
    if cli.webdriver {
        config.webdriver.enabled = true;
    }

    // Validate provider if specified
    if let Some(ref provider) = cli.provider {
        let valid_providers = ["anthropic", "databricks", "embedded", "openai"];
        if !valid_providers.contains(&provider.as_str()) {
            return Err(anyhow::anyhow!(
                "Invalid provider '{}'. Valid options: {:?}",
                provider,
                valid_providers
            ));
        }
    }

    // Initialize agent
    // ui_writer will be created conditionally based on machine mode
    
    // Combine AGENTS.md and README content if both exist
    let combined_content = match (agents_content.clone(), readme_content.clone()) {
        (Some(agents), Some(readme)) => {
            Some(format!("{}\n\n{}", agents, readme))
        }
        (Some(agents), None) => Some(agents),
        (None, Some(readme)) => Some(readme),
        (None, None) => None,
    };
    
    // Execute task, autonomous mode, or start interactive mode based on machine mode
    if cli.machine {
        // Machine mode - use MachineUiWriter
        if cli.accumulative {
            eprintln!("ERROR: --accumulative mode is not compatible with --machine mode");
            eprintln!("Please use either --accumulative or --machine, but not both.");
            std::process::exit(1);
        }
        
        
        let ui_writer = MachineUiWriter::new();
        
        let agent = if cli.autonomous {
            Agent::new_autonomous_with_readme_and_quiet(
                config.clone(),
                ui_writer,
                combined_content.clone(),
                cli.quiet,
            )
            .await?
        } else {
            // NEW DEFAULT: Accumulative mode for interactive sessions
            // It runs when:
            // 1. No task is provided (not single-shot)
            // 2. Not in autonomous mode
            // 3. Not explicitly disabled with --accumulative flag
            let use_accumulative = cli.task.is_none() && !cli.autonomous && !cli.accumulative;
            
            if use_accumulative {
                // Run accumulative mode and return early
                run_accumulative_mode(workspace_dir.clone(), cli.clone(), combined_content.clone()).await?;
                return Ok(());
            }
            
            Agent::new_with_readme_and_quiet(
                config.clone(),
                ui_writer,
                combined_content.clone(),
                cli.quiet,
            )
            .await?
        };
        
        run_with_machine_mode(agent, cli, project).await?;
    } else {
        // Normal mode - use ConsoleUiWriter
        let ui_writer = ConsoleUiWriter::new();
        
        let agent = if cli.autonomous {
            Agent::new_autonomous_with_readme_and_quiet(
                config.clone(),
                ui_writer,
                combined_content.clone(),
                cli.quiet,
            )
            .await?
        } else {
            Agent::new_with_readme_and_quiet(
                config.clone(),
                ui_writer,
                combined_content.clone(),
                cli.quiet,
            )
            .await?
        };
        
        run_with_console_mode(agent, cli, project, combined_content).await?;
    }
    
    Ok(())
}

/// Accumulative autonomous mode: accumulates requirements from user input
/// and runs autonomous mode after each input
async fn run_accumulative_mode(
    workspace_dir: PathBuf,
    cli: Cli,
    combined_content: Option<String>,
) -> Result<()> {
    let output = SimpleOutput::new();
    
    output.print("");
    output.print("🪿 G3 AI Coding Agent - Accumulative Mode");
    output.print("      >> describe what you want, I'll build it iteratively");
    output.print("");
    output.print(&format!("📁 Workspace: {}", workspace_dir.display()));
    output.print("");
    output.print("💡 Each input you provide will be added to requirements");
    output.print("   and I'll automatically work on implementing them.");
    output.print("");
    output.print("   Type 'exit' or 'quit' to stop, Ctrl+D to finish");
    output.print("");
    
    // Initialize rustyline editor with history
    let mut rl = DefaultEditor::new()?;
    let history_file = dirs::home_dir().map(|mut path| {
        path.push(".g3_accumulative_history");
        path
    });
    
    if let Some(ref history_path) = history_file {
        let _ = rl.load_history(history_path);
    }
    
    // Accumulated requirements stored in memory
    let mut accumulated_requirements = Vec::new();
    let mut turn_number = 0;
    
    loop {
        output.print(&format!("\n{}", "=".repeat(60)));
        if accumulated_requirements.is_empty() {
            output.print("📝 What would you like me to build? (describe your requirements)");
        } else {
            output.print(&format!("📝 Turn {} - What's next? (add more requirements or refinements)", turn_number + 1));
        }
        output.print(&format!("{}", "=".repeat(60)));
        
        let readline = rl.readline("requirement> ");
        match readline {
            Ok(line) => {
                let input = line.trim().to_string();
                
                if input.is_empty() {
                    continue;
                }
                
                if input == "exit" || input == "quit" {
                    output.print("\n👋 Goodbye!");
                    break;
                }
                
                // Add to history
                rl.add_history_entry(&input)?;
                
                // Add this requirement to accumulated list
                turn_number += 1;
                accumulated_requirements.push(format!("{}. {}", turn_number, input));
                
                // Build the complete requirements document
                let requirements_doc = format!(
                    "# Project Requirements\n\n\
                    ## Current Instructions and Requirements:\n\n\
                    {}\n\n\
                    ## Latest Requirement (Turn {}):\n\n\
                    {}",
                    accumulated_requirements.join("\n"),
                    turn_number,
                    input
                );
                
                output.print("");
                output.print(&format!("📋 Current instructions and requirements (Turn {}):", turn_number));
                output.print(&format!("   {}", input));
                output.print("");
                output.print("🚀 Starting autonomous implementation...");
                output.print("");
                
                // Create a project with the accumulated requirements
                let project = Project::new_autonomous_with_requirements(
                    workspace_dir.clone(),
                    requirements_doc.clone()
                )?;
                
                // Ensure workspace exists and enter it
                project.ensure_workspace_exists()?;
                project.enter_workspace()?;
                
                // Load configuration with CLI overrides
                let mut config = Config::load_with_overrides(
                    cli.config.as_deref(),
                    cli.provider.clone(),
                    cli.model.clone(),
                )?;
                
                // Apply macax flag override
                if cli.macax {
                    config.macax.enabled = true;
                }
                
                // Apply webdriver flag override
                if cli.webdriver {
                    config.webdriver.enabled = true;
                }
                
                // Create agent for this autonomous run
                let ui_writer = ConsoleUiWriter::new();
                let agent = Agent::new_autonomous_with_readme_and_quiet(
                    config.clone(),
                    ui_writer,
                    combined_content.clone(),
                    cli.quiet,
                )
                .await?;
                
                // Run autonomous mode with the accumulated requirements
                match run_autonomous(
                    agent,
                    project,
                    cli.show_prompt,
                    cli.show_code,
                    cli.max_turns,
                    cli.quiet,
                )
                .await
                {
                    Ok(_) => {
                        output.print("");
                        output.print("✅ Autonomous run completed");
                    }
                    Err(e) => {
                        output.print("");
                        output.print(&format!("❌ Autonomous run failed: {}", e));
                        output.print("   You can provide more requirements to continue.");
                    }
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => {
                output.print("\n👋 Goodbye!");
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
    
    Ok(())
}

// Simplified machine mode version of autonomous mode
async fn run_autonomous_machine(
    mut agent: Agent<MachineUiWriter>,
    project: Project,
    show_prompt: bool,
    show_code: bool,
    max_turns: usize,
    _quiet: bool,
) -> Result<()> {
    println!("AUTONOMOUS_MODE_STARTED");
    println!("WORKSPACE: {}", project.workspace().display());
    println!("MAX_TURNS: {}", max_turns);

    // Check if requirements exist
    if !project.has_requirements() {
        println!("ERROR: requirements.md not found in workspace directory");
        return Ok(());
    }

    // Read requirements
    let requirements = match project.read_requirements()? {
        Some(content) => content,
        None => {
            println!("ERROR: Could not read requirements");
            return Ok(());
        }
    };

    println!("REQUIREMENTS_LOADED");

    // For now, just execute a simple autonomous loop
    // This is a simplified version - full implementation would need coach-player loop
    let task = format!(
        "You are G3 in implementation mode. Read and implement the following requirements:\n\n{}\n\nImplement this step by step, creating all necessary files and code.",
        requirements
    );

    println!("TASK_START");
    let result = agent.execute_task_with_timing(&task, None, false, show_prompt, show_code, true).await?;
    println!("AGENT_RESPONSE:");
    println!("{}", result.response);
    println!("END_AGENT_RESPONSE");
    println!("TASK_END");

    println!("AUTONOMOUS_MODE_ENDED");
    Ok(())
}

async fn run_with_console_mode(
    mut agent: Agent<ConsoleUiWriter>,
    cli: Cli,
    project: Project,
    combined_content: Option<String>,
) -> Result<()> {

    // Execute task, autonomous mode, or start interactive mode
    if cli.autonomous {
        // Autonomous mode with coach-player feedback loop
        if !cli.machine {
            info!("Starting autonomous mode");
        }
        run_autonomous(
            agent,
            project,
            cli.show_prompt,
            cli.show_code,
            cli.max_turns,
            cli.quiet,
        )
        .await?;
    } else if let Some(task) = cli.task {
        // Single-shot mode
        if !cli.machine {
            info!("Executing task: {}", task);
        }
        let output = SimpleOutput::new();
        let result = agent
            .execute_task_with_timing(&task, None, false, cli.show_prompt, cli.show_code, true)
            .await?;
        output.print_smart(&result.response);
    } else {
        // Interactive mode (default)
        if !cli.machine {
            info!("Starting interactive mode");
        }
        println!("📁 Workspace: {}", project.workspace().display());
        run_interactive(agent, cli.show_prompt, cli.show_code, combined_content).await?;
    }

    Ok(())
}

async fn run_with_machine_mode(
    mut agent: Agent<MachineUiWriter>,
    cli: Cli,
    project: Project,
) -> Result<()> {
    if cli.autonomous {
        // Autonomous mode with coach-player feedback loop
        run_autonomous_machine(
            agent,
            project,
            cli.show_prompt,
            cli.show_code,
            cli.max_turns,
            cli.quiet,
        )
        .await?;
    } else if let Some(task) = cli.task {
        // Single-shot mode
        let result = agent
            .execute_task_with_timing(&task, None, false, cli.show_prompt, cli.show_code, true)
            .await?;
        println!("AGENT_RESPONSE:");
        println!("{}", result.response);
        println!("END_AGENT_RESPONSE");
    } else {
        // Interactive mode
        run_interactive_machine(agent, cli.show_prompt, cli.show_code).await?;
    }

    Ok(())
}

/// Check if we're in a project directory and read AGENTS.md if available
fn read_agents_config(workspace_dir: &Path) -> Option<String> {
    // Look for AGENTS.md in the current directory
    let agents_path = workspace_dir.join("AGENTS.md");
    
    if agents_path.exists() {
        match std::fs::read_to_string(&agents_path) {
            Ok(content) => {
                // Return the content with a note about which file was read
                info!("Loaded AGENTS.md from {}", agents_path.display());
                Some(format!(
                    "🤖 Agent Configuration (from AGENTS.md):\n\n{}",
                    content
                ))
            }
            Err(e) => {
                // Log the error but continue without the agents config
                error!("Failed to read AGENTS.md: {}", e);
                None
            }
        }
    } else {
        // Check for alternative names
        let alt_path = workspace_dir.join("agents.md");
        if alt_path.exists() {
            match std::fs::read_to_string(&alt_path) {
                Ok(content) => {
                    info!("Loaded agents.md from {}", alt_path.display());
                    Some(format!("🤖 Agent Configuration (from agents.md):\n\n{}", content))
                }
                Err(e) => {
                    error!("Failed to read agents.md: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }
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
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            let title = stripped.trim();
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

async fn run_interactive<W: UiWriter>(
    mut agent: Agent<W>,
    show_prompt: bool,
    show_code: bool,
    combined_content: Option<String>,
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

    // Display message if AGENTS.md or README was loaded
    if let Some(ref content) = combined_content {
        // Check what was loaded
        let has_agents = content.contains("Agent Configuration");
        let has_readme = content.contains("Project README");
        
        if has_agents {
            output.print("🤖 AGENTS.md configuration loaded");
        }
        
        if has_readme {
            // Extract the first heading or title from the README
            let readme_snippet = extract_readme_heading(content)
                .unwrap_or_else(|| "Project documentation loaded".to_string());

            output.print(&format!("📚 detected: {}", readme_snippet));
        }
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
                if let Some(without_backslash) = trimmed.strip_suffix('\\') {
                    // Remove the backslash and add to buffer
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

                    // Check for control commands
                    if input.starts_with('/') {
                        match input.as_str() {
                            "/help" => {
                                output.print("");
                                output.print("📖 Control Commands:");
                                output.print("  /compact   - Trigger auto-summarization (compacts conversation history)");
                                output.print("  /thinnify  - Trigger context thinning (replaces large tool results with file references)");
                                output.print("  /readme    - Reload README.md and AGENTS.md from disk");
                                output.print("  /stats     - Show detailed context and performance statistics");
                                output.print("  /help      - Show this help message");
                                output.print("  exit/quit  - Exit the interactive session");
                                output.print("");
                                continue;
                            }
                            "/compact" => {
                                output.print("🗜️ Triggering manual summarization...");
                                match agent.force_summarize().await {
                                    Ok(true) => {
                                        output.print("✅ Summarization completed successfully");
                                    }
                                    Ok(false) => {
                                        output.print("⚠️ Summarization failed");
                                    }
                                    Err(e) => {
                                        output.print(&format!("❌ Error during summarization: {}", e));
                                    }
                                }
                                continue;
                            }
                            "/thinnify" => {
                                let summary = agent.force_thin();
                                println!("{}", summary);
                                continue;
                            }
                            "/readme" => {
                                output.print("📚 Reloading README.md and AGENTS.md...");
                                match agent.reload_readme() {
                                    Ok(true) => output.print("✅ README content reloaded successfully"),
                                    Ok(false) => output.print("⚠️ No README was loaded at startup, cannot reload"),
                                    Err(e) => output.print(&format!("❌ Error reloading README: {}", e)),
                                }
                                continue;
                            }
                            "/stats" => {
                                let stats = agent.get_stats();
                                output.print(&stats);
                                continue;
                            }
                            _ => {
                                output.print(&format!("❌ Unknown command: {}. Type /help for available commands.", input));
                                continue;
                            }
                        }
                    }

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

async fn run_interactive_machine(
    mut agent: Agent<MachineUiWriter>,
    show_prompt: bool,
    show_code: bool,
) -> Result<()> {
    println!("INTERACTIVE_MODE_STARTED");

    // Display provider and model information
    match agent.get_provider_info() {
        Ok((provider, model)) => {
            println!("PROVIDER: {}", provider);
            println!("MODEL: {}", model);
        }
        Err(e) => {
            println!("ERROR: Failed to get provider info: {}", e);
        }
    }

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

    loop {
        let readline = rl.readline("");
        match readline {
            Ok(line) => {
                let input = line.trim().to_string();

                if input.is_empty() {
                    continue;
                }

                if input == "exit" || input == "quit" {
                    break;
                }

                // Add to history
                rl.add_history_entry(&input)?;

                // Check for control commands
                if input.starts_with('/') {
                    match input.as_str() {
                        "/compact" => {
                            println!("COMMAND: compact");
                            match agent.force_summarize().await {
                                Ok(true) => println!("RESULT: Summarization completed"),
                                Ok(false) => println!("RESULT: Summarization failed"),
                                Err(e) => println!("ERROR: {}", e),
                            }
                            continue;
                        }
                        "/thinnify" => {
                            println!("COMMAND: thinnify");
                            let summary = agent.force_thin();
                            println!("{}", summary);
                            continue;
                        }
                        "/readme" => {
                            println!("COMMAND: readme");
                            match agent.reload_readme() {
                                Ok(true) => println!("RESULT: README content reloaded successfully"),
                                Ok(false) => println!("RESULT: No README was loaded at startup, cannot reload"),
                                Err(e) => println!("ERROR: {}", e),
                            }
                            continue;
                        }
                        "/stats" => {
                            println!("COMMAND: stats");
                            let stats = agent.get_stats();
                            // Emit stats as structured data (name: value pairs)
                            println!("{}", stats);
                            continue;
                        }
                        "/help" => {
                            println!("COMMAND: help");
                            println!("AVAILABLE_COMMANDS: /compact /thinnify /readme /stats /help");
                            continue;
                        }
                        _ => {
                            println!("ERROR: Unknown command: {}", input);
                            continue;
                        }
                    }
                }

                // Execute task
                println!("TASK_START");
                execute_task_machine(&mut agent, &input, show_prompt, show_code).await;
                println!("TASK_END");
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(err) => {
                println!("ERROR: {:?}", err);
                break;
            }
        }
    }

    // Save history before exiting
    if let Some(ref history_path) = history_file {
        let _ = rl.save_history(history_path);
    }

    println!("INTERACTIVE_MODE_ENDED");
    Ok(())
}

async fn execute_task_machine(
    agent: &mut Agent<MachineUiWriter>,
    input: &str,
    show_prompt: bool,
    show_code: bool,
) {
    const MAX_TIMEOUT_RETRIES: u32 = 3;
    let mut attempt = 0;

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
                println!("CANCELLED");
                return;
            }
        };

        match execution_result {
            Ok(result) => {
                if attempt > 1 {
                    println!("RETRY_SUCCESS: attempt {}", attempt);
                }
                println!("AGENT_RESPONSE:");
                println!("{}", result.response);
                println!("END_AGENT_RESPONSE");
                return;
            }
            Err(e) => {
                if e.to_string().contains("cancelled") {
                    println!("CANCELLED");
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

                    println!("TIMEOUT: attempt {} of {}, retrying in {:?}", attempt, MAX_TIMEOUT_RETRIES, delay);

                    // Wait before retrying
                    tokio::time::sleep(delay).await;
                    continue;
                }

                // For non-timeout errors or after max retries
                println!("ERROR: {}", e);
                if attempt > 1 {
                    println!("FAILED_AFTER_RETRIES: {}", attempt);
                }
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
    output.print(&format!("Context: {}/{} tokens ({:.1}%)", 
        context.used_tokens, context.total_tokens, context.percentage_used()));
}

/// Set up the workspace directory for autonomous mode
/// Uses G3_WORKSPACE environment variable or defaults to ~/tmp/workspace
fn setup_workspace_directory(machine_mode: bool) -> Result<PathBuf> {
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
        let output = SimpleOutput::new_with_mode(machine_mode);
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
    quiet: bool,
) -> Result<()> {
    let start_time = std::time::Instant::now();
    let output = SimpleOutput::new();
    let mut turn_metrics: Vec<TurnMetrics> = Vec::new();

    output.print("🤖 G3 AI Coding Agent - Autonomous Mode");
    output.print(&format!(
        "📁 Using workspace: {}",
        project.workspace().display()
    ));

    // Check if requirements exist
    if !project.has_requirements() {
        output.print("❌ Error: requirements.md not found in workspace directory");
        output.print("   Please either:");
        output.print("   1. Create a requirements.md file with your project requirements at:");
        output.print(&format!(
            "      {}/requirements.md",
            project.workspace().display()
        ));
        output.print("   2. Or use the --requirements flag to provide requirements text directly:");
        output.print("      g3 --autonomous --requirements \"Your requirements here\"");
        output.print("");

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
        output.print("📝 Final Status: ⚠️ NO REQUIREMENTS FILE");

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
        // Add per-turn histogram
        output.print(&generate_turn_histogram(&turn_metrics));
        output.print(&"=".repeat(60));

        return Ok(());
    }

    // Read requirements
    let requirements = match project.read_requirements()? {
        Some(content) => content,
        None => {
            output.print("❌ Error: Could not read requirements (neither --requirements flag nor requirements.md file provided)");

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
            output.print("📝 Final Status: ⚠️ CANNOT READ REQUIREMENTS");

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
            // Add per-turn histogram
            output.print(&generate_turn_histogram(&turn_metrics));
            output.print(&"=".repeat(60));

            return Ok(());
        }
    };

    // Display appropriate message based on requirements source
    if project.requirements_text.is_some() {
        output.print("📋 Requirements loaded from --requirements flag");
    } else {
        output.print("📋 Requirements loaded from requirements.md");
    }
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
        let turn_start_time = Instant::now();
        let turn_start_tokens = agent.get_context_window().used_tokens;
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
                    return Err(anyhow::anyhow!(
                        "Player mode error: No coach feedback received on turn {}",
                        turn
                    ));
                }
                output.print("📋 Player starting initial implementation (no prior coach feedback)");
            } else {
                output.print(&format!(
                    "📋 Player received coach feedback ({} chars):",
                    coach_feedback.len()
                ));
                output.print(&coach_feedback.to_string());
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
                            output.print("📝 Final Status: 💥 PLAYER PANIC");

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
                            // Add per-turn histogram
                            output.print(&generate_turn_histogram(&turn_metrics));
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
                // Record turn metrics before incrementing
                let turn_duration = turn_start_time.elapsed();
                let turn_tokens = agent.get_context_window().used_tokens.saturating_sub(turn_start_tokens);
                turn_metrics.push(TurnMetrics {
                    turn_number: turn,
                    tokens_used: turn_tokens,
                    wall_clock_time: turn_duration,
                });
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
        let base_config = agent.get_config().clone();
        let coach_config = base_config.for_coach()?;

        // Reset filter suppression state before creating coach agent
        g3_core::fixed_filter_json::reset_fixed_json_tool_state();

        let ui_writer = ConsoleUiWriter::new();
        let mut coach_agent =
            Agent::new_autonomous_with_readme_and_quiet(coach_config, ui_writer, None, quiet).await?;

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
5. Use UI tools such as webdriver or macax to test functionality thoroughly

CRITICAL INSTRUCTIONS:
1. You MUST use the final_output tool to provide your feedback
2. The summary in final_output should be CONCISE and ACTIONABLE
3. Focus ONLY on what needs to be fixed or improved
4. Do NOT include your analysis process, file contents, or compilation output in the summary

If the implementation thoroughly meets all requirements, compiles and is fully tested (especially UI flows) *WITHOUT* minor gaps or errors:
- Call final_output with summary: 'IMPLEMENTATION_APPROVED'

If improvements are needed:
- Call final_output with a brief summary listing ONLY the specific issues to fix

Remember: Be clear in your review and concise in your feedback. APPROVE iff the implementation works and thoroughly fits the requirements (implementation > 95% complete). Be rigorous, especially by testing that all UI features work.",
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
                        output.print("📝 Final Status: 💥 COACH PANIC");

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
                        // Add per-turn histogram
                        output.print(&generate_turn_histogram(&turn_metrics));
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
            // Record turn metrics before incrementing
            let turn_duration = turn_start_time.elapsed();
            let turn_tokens = agent.get_context_window().used_tokens.saturating_sub(turn_start_tokens);
            turn_metrics.push(TurnMetrics {
                turn_number: turn,
                tokens_used: turn_tokens,
                wall_clock_time: turn_duration,
            });
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
        let coach_feedback_text =
            extract_coach_feedback_from_logs(&coach_result, &coach_agent, &output)?;

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
            // Record turn metrics before incrementing
            let turn_duration = turn_start_time.elapsed();
            let turn_tokens = agent.get_context_window().used_tokens.saturating_sub(turn_start_tokens);
            turn_metrics.push(TurnMetrics {
                turn_number: turn,
                tokens_used: turn_tokens,
                wall_clock_time: turn_duration,
            });
            turn += 1;
            continue;
        }

        output.print_smart(&format!("Coach feedback:\n{}", coach_feedback_text));

        // Check if coach approved the implementation
        if coach_result.is_approved() || coach_feedback_text.contains("IMPLEMENTATION_APPROVED") {
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
        // Record turn metrics before incrementing
        let turn_duration = turn_start_time.elapsed();
        let turn_tokens = agent.get_context_window().used_tokens.saturating_sub(turn_start_tokens);
        turn_metrics.push(TurnMetrics {
            turn_number: turn,
            tokens_used: turn_tokens,
            wall_clock_time: turn_duration,
        });
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
    
    // Add per-turn histogram
    output.print(&generate_turn_histogram(&turn_metrics));
    output.print(&"=".repeat(60));

    if implementation_approved {
        output.print("\n🎉 Autonomous mode completed successfully");
    } else {
        output.print("\n🔄 Autonomous mode terminated (max iterations)");
    }

    Ok(())
}
