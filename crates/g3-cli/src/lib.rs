use anyhow::Result;
use clap::Parser;
use g3_config::Config;
use g3_core::Agent;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

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

    /// Task to execute (if provided, runs in single-shot mode instead of interactive)
    pub task: Option<String>,

    /// Enable autonomous mode with coach-player feedback loop
    #[arg(long)]
    pub autonomous: bool,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

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

    info!("Starting G3 AI Coding Agent");

    // Load configuration
    let config = Config::load(cli.config.as_deref())?;

    // Initialize agent
    let mut agent = Agent::new(config).await?;

    // Execute task, autonomous mode, or start interactive mode
    if cli.autonomous {
        // Autonomous mode with coach-player feedback loop
        info!("Starting autonomous mode");
        run_autonomous(agent, cli.show_prompt, cli.show_code).await?;
    } else if let Some(task) = cli.task {
        // Single-shot mode
        info!("Executing task: {}", task);
        let result = agent
            .execute_task_with_timing(&task, None, false, cli.show_prompt, cli.show_code, true)
            .await?;
        println!("{}", result);
    } else {
        // Interactive mode (default)
        info!("Starting interactive mode");
        run_interactive(agent, cli.show_prompt, cli.show_code).await?;
    }

    Ok(())
}

async fn run_interactive(mut agent: Agent, show_prompt: bool, show_code: bool) -> Result<()> {
    println!("🤖 G3 AI Coding Agent - Interactive Mode");
    println!(
        "I solve problems by writing and executing code. Tell me what you need to accomplish!"
    );
    println!();

    // Display provider and model information
    match agent.get_provider_info() {
        Ok((provider, model)) => {
            println!("🔧 Provider: {} | Model: {}", provider, model);
        }
        Err(e) => {
            error!("Failed to get provider info: {}", e);
        }
    }

    println!();
    println!("Type 'exit' or 'quit' to exit, use Up/Down arrows for command history");
    println!();

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
        // Display context window progress bar before each prompt
        display_context_progress(&agent);

        let readline = rl.readline("g3> ");
        match readline {
            Ok(line) => {
                let input = line.trim();

                if input == "exit" || input == "quit" {
                    break;
                }

                if input.is_empty() {
                    continue;
                }

                // Add to history
                rl.add_history_entry(input)?;

                // Show thinking indicator immediately
                print!("🤔 Thinking...");
                std::io::stdout().flush()?;

                // Create cancellation token for this request
                let cancellation_token = CancellationToken::new();
                let cancel_token_clone = cancellation_token.clone();

                // Spawn a task to monitor for ESC key during execution
                let esc_monitor = tokio::spawn(async move {
                    // This is a simplified approach - in a real implementation,
                    // we'd need to handle raw terminal input to detect ESC
                    // For now, we'll just provide the cancellation infrastructure
                    tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                });

                // Execute task with cancellation support
                let execution_result = tokio::select! {
                    result = agent.execute_task_with_timing_cancellable(
                        input, None, false, show_prompt, show_code, true, cancellation_token
                    ) => {
                        esc_monitor.abort();
                        result
                    }
                    _ = tokio::signal::ctrl_c() => {
                        cancel_token_clone.cancel();
                        esc_monitor.abort();
                        println!("\n⚠️  Operation cancelled by user (Ctrl+C)");
                        continue;
                    }
                };

                match execution_result {
                    Ok(response) => println!("{}", response),
                    Err(e) => {
                        if e.to_string().contains("cancelled") {
                            println!("⚠️  Operation cancelled by user");
                        } else {
                            error!("Error: {}", e);
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
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

    println!("👋 Goodbye!");
    Ok(())
}

async fn run_autonomous(mut agent: Agent, show_prompt: bool, show_code: bool) -> Result<()> {
    // Set up workspace directory
    let workspace_dir = setup_workspace_directory()?;

    // Set up logging
    let logger = AutonomousLogger::new(&workspace_dir)?;

    logger.log_section("G3 AUTONOMOUS MODE SESSION STARTED");
    logger.log(&format!("🤖 G3 AI Coding Agent - Autonomous Mode"));
    logger.log(&format!(
        "📁 Using workspace directory: {}",
        workspace_dir.display()
    ));

    // Change to workspace directory
    std::env::set_current_dir(&workspace_dir)?;
    logger.log("📂 Changed to workspace directory");

    logger.log("🎯 Looking for requirements.md in workspace directory...");

    // Check if requirements.md exists
    let requirements_path = workspace_dir.join("requirements.md");
    if !requirements_path.exists() {
        logger.log("❌ Error: requirements.md not found in workspace directory");
        logger.log(&format!(
            "   Please create a requirements.md file with your project requirements at:"
        ));
        logger.log(&format!("   {}", requirements_path.display()));
        return Ok(());
    }

    // Read requirements.md
    let requirements = match std::fs::read_to_string(&requirements_path) {
        Ok(content) => content,
        Err(e) => {
            logger.log(&format!("❌ Error reading requirements.md: {}", e));
            return Ok(());
        }
    };

    logger.log("📋 Requirements loaded from requirements.md");
    logger.log(&format!(
        "Requirements: {}",
        logger.truncate_for_log(&requirements, 150)
    ));

    // Check if there are existing project files (skip first player turn if so)
    let has_existing_files = check_existing_project_files(&workspace_dir, &logger)?;

    logger.log("🔄 Starting coach-player feedback loop...");
    logger.log("");

    const MAX_TURNS: usize = 5;
    let mut turn = 1;
    let mut coach_feedback = String::new();
    let mut skip_player_turn = has_existing_files;

    loop {
        // Skip player turn if we have existing files and this is the first iteration
        if skip_player_turn {
            logger.log_section(&format!(
                "TURN {}/{} - SKIPPING PLAYER MODE",
                turn, MAX_TURNS
            ));
            logger.log("📁 Existing project files detected, skipping to coach evaluation");
            skip_player_turn = false; // Only skip the first turn
        } else {
            logger.log_section(&format!("TURN {}/{} - PLAYER MODE", turn, MAX_TURNS));

            // Player mode: implement requirements (with coach feedback if available)
            let player_prompt = if coach_feedback.is_empty() {
                format!(
                    "You are G3 in implementation mode. Read and implement the following requirements:\n\n{}\n\nImplement this step by step, creating all necessary files and code.",
                    requirements
                )
            } else {
                format!(
                    "You are G3 in implementation mode. You need to address the coach's feedback and improve your implementation.\n\nORIGINAL REQUIREMENTS:\n{}\n\nCOACH FEEDBACK TO ADDRESS:\n{}\n\nPlease make the necessary improvements to address the coach's feedback while ensuring all original requirements are met.",
                    requirements, coach_feedback
                )
            };

            logger.log("🎯 Starting player implementation...");
            if !coach_feedback.is_empty() {
                logger.log("📝 Incorporating coach feedback from previous turn");
            }

            let _player_result = agent
                .execute_task_with_timing(&player_prompt, None, false, show_prompt, show_code, true)
                .await?;

            logger.log("🎯 Player implementation completed");
            logger.log("");
        }

        // Create a new agent instance for coach mode to ensure fresh context
        // Make sure the coach agent also operates in the workspace directory
        let config = g3_config::Config::load(None)?;
        let mut coach_agent = Agent::new(config).await?;

        // Ensure coach agent is also in the workspace directory
        std::env::set_current_dir(&workspace_dir)?;

        logger.log_section(&format!("TURN {}/{} - COACH MODE", turn, MAX_TURNS));

        // Coach mode: critique the implementation
        let coach_prompt = format!(
            "You are G3 in coach mode. Your role is to critique and review implementations against requirements.

REQUIREMENTS:
{}

IMPLEMENTATION REVIEW:
Review the current state of the project and provide a concise critique focusing on:
1. Whether the requirements are correctly implemented
2. What's missing or incorrect
3. Specific improvements needed

If the implementation correctly meets all requirements, respond with: 'IMPLEMENTATION_APPROVED'
If improvements are needed, provide specific actionable feedback. Don't be overly critical. APPROVE the
implementation if it generally fits the bill, doesn't have compile errors or glaring omissions.

Keep your response concise and focused on actionable items.",
            requirements
        );

        logger.log("🎓 Starting coach review...");

        let coach_result = coach_agent
            .execute_task_with_timing(&coach_prompt, None, false, show_prompt, show_code, true)
            .await?;

        logger.log("🎓 Coach review completed");
        logger.log(&format!("Coach feedback: {}", coach_result));

        // Check if coach approved the implementation
        if coach_result.contains("IMPLEMENTATION_APPROVED") {
            logger.log_section("SESSION COMPLETED - IMPLEMENTATION APPROVED");
            logger.log("✅ Coach approved the implementation!");
            logger.log("🎉 Autonomous mode completed successfully");
            break;
        }

        // Check if we've reached max turns
        if turn >= MAX_TURNS {
            logger.log_section("SESSION COMPLETED - MAX TURNS REACHED");
            logger.log(&format!("⏰ Maximum turns ({}) reached", MAX_TURNS));
            logger.log("🔄 Autonomous mode completed (max iterations)");
            break;
        }

        // Store coach feedback for next iteration
        coach_feedback = coach_result;
        turn += 1;

        logger.log("🔄 Coach provided feedback for next iteration");
        logger.log(&format!(
            "📝 Preparing to incorporate feedback in turn {}",
            turn
        ));
        logger.log("");
    }

    logger.log_section("G3 AUTONOMOUS MODE SESSION ENDED");
    Ok(())
}

/// Check if there are existing project files in the workspace directory
/// Returns true if project files are found (excluding requirements.md and logs directory)
fn check_existing_project_files(
    workspace_dir: &PathBuf,
    logger: &AutonomousLogger,
) -> Result<bool> {
    logger.log("🔍 Checking for existing project files...");

    let entries = match std::fs::read_dir(workspace_dir) {
        Ok(entries) => entries,
        Err(e) => {
            logger.log(&format!("❌ Failed to read workspace directory: {}", e));
            return Ok(false);
        }
    };

    let mut project_files = Vec::new();
    let mut total_files = 0;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // Skip requirements.md, logs directory, and hidden files
        if file_name == "requirements.md" || file_name == "logs" || file_name.starts_with('.') {
            continue;
        }

        total_files += 1;

        // Collect project files for logging (limit to first 5)
        if project_files.len() < 5 {
            if path.is_dir() {
                project_files.push(format!("{}/", file_name));
            } else {
                project_files.push(file_name.to_string());
            }
        }
    }

    if total_files > 0 {
        logger.log(&format!("📁 Found {} existing project files", total_files));
        if !project_files.is_empty() {
            let files_display = if total_files > 5 {
                format!(
                    "{} (and {} more)",
                    project_files.join(", "),
                    total_files - 5
                )
            } else {
                project_files.join(", ")
            };
            logger.log(&format!("   Files: {}", files_display));
        }
        logger.log("⏭️  Will skip first player turn and evaluate existing implementation");
        Ok(true)
    } else {
        logger.log("📂 No existing project files found, starting fresh implementation");
        Ok(false)
    }
}

fn display_context_progress(agent: &Agent) {
    let context = agent.get_context_window();
    let percentage = context.percentage_used();

    // Create a simple visual progress bar using the requested characters (10 dots max)
    let bar_width = 10;
    let filled_width = ((percentage / 100.0) * bar_width as f32) as usize;
    let empty_width = bar_width - filled_width;

    let filled_chars = "●".repeat(filled_width);
    let empty_chars = "○".repeat(empty_width);
    let progress_bar = format!("{}{}", filled_chars, empty_chars);

    // Print context info with visual progress bar
    println!(
        "Context: {} {:.1}% | {}/{} tokens",
        progress_bar, percentage, context.used_tokens, context.total_tokens
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
        println!(
            "📁 Created workspace directory: {}",
            workspace_dir.display()
        );
    }

    Ok(workspace_dir)
}

/// Logger for autonomous mode that writes to both console and log file
struct AutonomousLogger {
    log_writer: Arc<Mutex<BufWriter<std::fs::File>>>,
}

impl AutonomousLogger {
    fn new(workspace_dir: &PathBuf) -> Result<Self> {
        // Create logs subdirectory
        let logs_dir = workspace_dir.join("logs");
        if !logs_dir.exists() {
            std::fs::create_dir_all(&logs_dir)?;
        }

        // Create log file with timestamp in logs subdirectory
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let log_path = logs_dir.join(format!("g3_autonomous_{}.log", timestamp));

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&log_path)?;

        let log_writer = Arc::new(Mutex::new(BufWriter::new(file)));

        println!("📝 Logging autonomous session to: {}", log_path.display());

        Ok(Self { log_writer })
    }

    /// Truncate text to a single line for logging
    fn truncate_for_log(&self, text: &str, max_chars: usize) -> String {
        // First, get the first line only
        let first_line = text.lines().next().unwrap_or("").trim();

        // Then truncate if too long
        if first_line.len() <= max_chars {
            first_line.to_string()
        } else {
            format!("{}...", &first_line[..max_chars.saturating_sub(3)])
        }
    }

    fn log(&self, message: &str) {
        // Ensure single line for console output
        let single_line_message = self.truncate_for_log(message, 200);

        // Print to console
        println!("{}", single_line_message);

        // Write to log file with timestamp (also single line)
        if let Ok(mut writer) = self.log_writer.lock() {
            let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
            let _ = writeln!(writer, "[{}] {}", timestamp, single_line_message);
            let _ = writer.flush();
        }
    }

    fn log_section(&self, section: &str) {
        // Sections can be multi-line for visual separation, but content should be single line
        let single_line_section = self.truncate_for_log(section, 100);
        let separator = "=".repeat(80);

        // Print to console with visual formatting
        println!("{}", separator);
        println!("{}", single_line_section);
        println!("{}", separator);

        // Log to file as single entries
        if let Ok(mut writer) = self.log_writer.lock() {
            let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
            let _ = writeln!(writer, "[{}] === {} ===", timestamp, single_line_section);
            let _ = writer.flush();
        }
    }

    fn log_subsection(&self, subsection: &str) {
        let single_line_subsection = self.truncate_for_log(subsection, 100);
        let separator = "-".repeat(60);

        // Print to console with visual formatting
        println!("{}", separator);
        println!("{}", single_line_subsection);
        println!("{}", separator);

        // Log to file as single entry
        if let Ok(mut writer) = self.log_writer.lock() {
            let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
            let _ = writeln!(writer, "[{}] --- {} ---", timestamp, single_line_subsection);
            let _ = writer.flush();
        }
    }
}
