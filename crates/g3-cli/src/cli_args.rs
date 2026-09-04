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
    /// Override the workspace memory file location (default: analysis/memory.md).
    /// Governs both startup loading and where the `remember` tool writes.
    pub memory_path: Option<PathBuf>,
    /// Disable automatic memory update reminder
    pub no_auto_memory: bool,
    /// Enable aggressive context dehydration
    pub acd: bool,
    /// Override `agent.thinning_floor_percent` for this run (e.g. scout's
    /// aggressive 5% floor to discard webdriver HTML right after each tool
    /// call). `None` leaves the config file / default (50) untouched.
    pub thinning_floor: Option<u32>,
    /// Load a project from the given path at startup
    pub project: Option<PathBuf>,
    /// Resume a specific session by ID
    pub resume: Option<String>,
    /// Emit structured NDJSON events (tokens, tool calls, results) to this file path.
    /// Used by external UIs (e.g. butler.app) to display live streaming without
    /// re-implementing g3's markdown/JSON-filter state machines.
    pub stream_events: Option<PathBuf>,
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

    /// On a "model overloaded" error, retry the current turn with this model
    /// instead, then revert to the default model on the very next turn.
    ///
    /// Bare `--fallback-model` uses claude-opus-4-8. Pass a specific model with
    /// `--fallback-model=<MODEL>`.
    ///
    /// NOTE the `require_equals`: without it, clap would treat `g3
    /// --fallback-model "do the thing"` as *model = "do the thing"* and silently
    /// eat the positional task argument. Requiring `=` makes the bare flag
    /// unambiguous.
    #[arg(
        long,
        value_name = "MODEL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = g3_config::DEFAULT_FALLBACK_MODEL
    )]
    pub fallback_model: Option<String>,

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

    /// Override the thinning floor (percent of context window at which
    /// incremental thinning first becomes eligible; default 50, see
    /// `agent.thinning_floor_percent`). Used by the `scout` research agent
    /// with a value of 5 so large webdriver page-source dumps are discarded
    /// right after the tool call that produced them, instead of accumulating
    /// toward compaction.
    #[arg(long, value_name = "PERCENT")]
    pub thinning_floor: Option<u32>,

    /// Include additional prompt content from a file (appended before memory)
    #[arg(long, value_name = "PATH")]
    pub include_prompt: Option<PathBuf>,

    /// Override the workspace memory file (default: <workspace>/analysis/memory.md).
    /// Governs BOTH startup loading and where the `remember` tool writes, so memory
    /// cannot fork. Useful to keep personal memory outside a git repo.
    #[arg(long, value_name = "PATH")]
    pub memory: Option<PathBuf>,

    /// Disable automatic memory update reminder at end of agent mode
    #[arg(long)]
    pub no_auto_memory: bool,

    /// Load a project from the given path at startup (like /project but without auto-prompt)
    #[arg(long, value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Emit structured NDJSON events (streaming tokens, tool calls, tool results) to this file.
    /// Consumed by external UIs (butler.app) — see docs/stream-events.md.
    #[arg(long, value_name = "PATH")]
    pub stream_events: Option<PathBuf>,
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
            memory_path: self.memory.clone(),
            no_auto_memory: self.no_auto_memory,
            acd: self.acd,
            thinning_floor: self.thinning_floor,
            project: self.project.clone(),
            resume: self.resume.clone(),
            stream_events: self.stream_events.clone(),
        }
    }
}

#[cfg(test)]
mod fallback_model_flag_tests {
    use super::*;
    use clap::Parser;

    /// Parse an argv (without the binary name) as a `Cli`.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["g3"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn test_flag_absent_means_no_fallback() {
        let cli = parse(&[]).expect("bare g3 should parse");
        assert_eq!(cli.fallback_model, None);
    }

    #[test]
    fn test_bare_flag_uses_default_model() {
        let cli = parse(&["--fallback-model"]).expect("bare flag should parse");
        assert_eq!(
            cli.fallback_model.as_deref(),
            Some(g3_config::DEFAULT_FALLBACK_MODEL)
        );
    }

    #[test]
    fn test_explicit_model_overrides_default() {
        let cli = parse(&["--fallback-model=claude-sonnet-5"]).expect("should parse");
        assert_eq!(cli.fallback_model.as_deref(), Some("claude-sonnet-5"));
    }

    /// THE REGRESSION THIS FLAG SHAPE EXISTS TO PREVENT.
    ///
    /// Without `require_equals`, clap binds the next token to the optional
    /// value, so `g3 --fallback-model "do the thing"` would set
    /// fallback_model = "do the thing" and leave `task` as None — the task
    /// silently vanishes and g3 drops into interactive mode instead of running
    /// it. Assert the task survives AND the fallback is the default.
    #[test]
    fn test_bare_flag_does_not_swallow_positional_task() {
        let cli = parse(&["--fallback-model", "do the thing"]).expect("should parse");
        assert_eq!(
            cli.task.as_deref(),
            Some("do the thing"),
            "positional task must not be consumed as the fallback model value"
        );
        assert_eq!(
            cli.fallback_model.as_deref(),
            Some(g3_config::DEFAULT_FALLBACK_MODEL)
        );
    }

    /// Space-separated values are rejected outright rather than mis-bound.
    #[test]
    fn test_space_separated_value_is_not_accepted_as_model() {
        let cli = parse(&["--fallback-model", "claude-sonnet-5"]).expect("should parse");
        // "claude-sonnet-5" lands in the positional task slot, NOT the model.
        assert_eq!(
            cli.fallback_model.as_deref(),
            Some(g3_config::DEFAULT_FALLBACK_MODEL)
        );
        assert_eq!(cli.task.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn test_empty_explicit_value_parses_as_empty_string() {
        // Boundary: `--fallback-model=` is degenerate but must not panic.
        let cli = parse(&["--fallback-model="]).expect("should parse");
        assert_eq!(cli.fallback_model.as_deref(), Some(""));
    }

    #[test]
    fn test_flag_coexists_with_model_and_provider_overrides() {
        let cli = parse(&[
            "--provider",
            "anthropic",
            "--model",
            "claude-opus-5",
            "--fallback-model=claude-opus-4-8",
        ])
        .expect("should parse");
        assert_eq!(cli.provider.as_deref(), Some("anthropic"));
        assert_eq!(cli.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(cli.fallback_model.as_deref(), Some("claude-opus-4-8"));
    }
}
