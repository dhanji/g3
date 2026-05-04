//! CLI argument parsing for G3.

use clap::Parser;
use std::path::PathBuf;

/// Flags that apply across all execution modes (interactive, agent, autonomous).
/// 
/// When adding a new flag that should work in all modes, add it here instead of
/// passing individual parameters to mode functions. This prevents bugs where a
/// flag works in one mode but is forgotten in another.
#[derive(Clone, Debug, Default)]
pub struct CommonFlags {
    /// Workspace directory
    pub workspace: Option<PathBuf>,
    /// Configuration file path
    pub config: Option<String>,
    /// Skip session resumption and force a new session
    pub new_session: bool,
    /// Suppress output/logging
    pub quiet: bool,
    /// Use Chrome in headless mode for WebDriver
    pub chrome_headless: bool,
    /// Use Safari for WebDriver
    pub safari: bool,
    /// Include additional prompt content from a file
    pub include_prompt: Option<PathBuf>,
    /// Disable automatic memory update reminder
    pub no_auto_memory: bool,
    /// Enable aggressive context dehydration
    pub acd: bool,
    /// Load a project from the given path at startup
    pub project: Option<PathBuf>,
    /// Resume a specific session by ID
    pub resume: Option<String>,
    /// Skip the plan approval gate for plan tools
    pub skip_plan_tool_check: bool,
}

#[derive(Parser, Clone)]
#[command(name = "g3")]
#[command(about = "A modular, composable AI coding agent")]
#[command(version)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Enable manual control of context compaction (disables auto-compact at 90%)
    #[arg(long = "manual-compact")]
    pub manual_compact: bool,

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

    /// Enable accumulative autonomous mode (default is chat mode)
    #[arg(long)]
    pub auto: bool,

    /// Enable interactive chat mode (no autonomous runs)
    #[arg(long)]
    pub chat: bool,

    /// Override the configured provider (e.g., 'openai' or 'openai.default')
    #[arg(long, value_name = "PROVIDER")]
    pub provider: Option<String>,

    /// Override the model for the selected provider
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Disable session log file creation (no .g3/sessions/ or error logs)
    #[arg(long)]
    pub quiet: bool,

    /// Enable WebDriver browser automation tools
    #[arg(long, default_value_t = true)]
    pub webdriver: bool,

    /// Use Chrome in headless mode for WebDriver (instead of Safari)
    #[arg(long, default_value_t = true)]
    pub chrome_headless: bool,

    /// Use Safari for WebDriver (overrides the default Chrome headless)
    #[arg(long)]
    pub safari: bool,

    /// Enable planning mode for requirements-driven development
    #[arg(long, conflicts_with_all = ["autonomous", "auto", "chat"])]
    pub planning: bool,

    /// Path to the codebase to work on (for planning mode)
    #[arg(long, value_name = "PATH")]
    pub codepath: Option<String>,

    /// Disable git operations in planning mode
    #[arg(long)]
    pub no_git: bool,

    /// Enable fast codebase discovery before first LLM turn
    #[arg(long, value_name = "PATH")]
    pub codebase_fast_start: Option<PathBuf>,

    /// Run as a specialized agent (loads prompt from agents/<name>.md)
    #[arg(long, value_name = "NAME", conflicts_with_all = ["autonomous", "auto", "planning"])]
    pub agent: Option<String>,

    /// List all available agents (embedded and workspace)
    #[arg(long)]
    pub list_agents: bool,

    /// Skip session resumption and force a new session (for agent mode)
    #[arg(long)]
    pub new_session: bool,

    /// Resume a specific session by ID (full or partial prefix)
    #[arg(long, value_name = "SESSION_ID", conflicts_with = "new_session")]
    pub resume: Option<String>,

    /// Automatically remind LLM to call remember tool after turns with tool calls
    #[arg(long)]
    pub auto_memory: bool,

    /// Enable aggressive context dehydration (save context to disk on compaction)
    #[arg(long)]
    pub acd: bool,

    /// Include additional prompt content from a file (appended before memory)
    #[arg(long, value_name = "PATH")]
    pub include_prompt: Option<PathBuf>,

    /// Disable automatic memory update reminder at end of agent mode
    #[arg(long)]
    pub no_auto_memory: bool,

    /// Load a project from the given path at startup (like /project but without auto-prompt)
    #[arg(long, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Skip the plan approval gate check for plan tools (plan_read, plan_write, plan_approve).
    /// Without this flag, plan tools are subject to the same approval gate as other tools.
    #[arg(long)]
    pub skip_plan_tool_check: bool,
}

impl Cli {
    /// Extract common flags that apply across all execution modes.
    /// This ensures flags like --project, --acd, --include-prompt work consistently.
    pub fn common_flags(&self) -> CommonFlags {
        CommonFlags {
            workspace: self.workspace.clone(),
            config: self.config.clone(),
            new_session: self.new_session,
            quiet: self.quiet,
            chrome_headless: self.chrome_headless,
            safari: self.safari,
            include_prompt: self.include_prompt.clone(),
            no_auto_memory: self.no_auto_memory,
            acd: self.acd,
            project: self.project.clone(),
            resume: self.resume.clone(),
            skip_plan_tool_check: self.skip_plan_tool_check,
        }
    }
}
