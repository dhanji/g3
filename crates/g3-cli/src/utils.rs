//! Utility functions for G3 CLI.

use anyhow::Result;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use g3_config::Config;
use std::path::PathBuf;

use crate::cli_args::Cli;
use crate::simple_output::SimpleOutput;

/// Render the context-window dot bar with color + compact token counts.
/// Standalone so both the interactive prompt and end-of-turn summary use it.
pub fn render_context_bar(used_tokens: u32, total_tokens: u32, percentage: f32) {
    // Ensure we start on a new line (previous response may not end with newline)
    println!();

    // Create 10 dots representing context fullness
    let total_dots: usize = 10;
    let filled_dots = ((percentage / 100.0) * total_dots as f32).round() as usize;
    let empty_dots = total_dots.saturating_sub(filled_dots);

    let filled_str = "●".repeat(filled_dots);
    let empty_str = "○".repeat(empty_dots);

    // Determine color based on percentage
    let color = if percentage < 40.0 {
        Color::Green
    } else if percentage < 60.0 {
        Color::Yellow
    } else if percentage < 80.0 {
        Color::Rgb {
            r: 255,
            g: 165,
            b: 0,
        } // Orange
    } else {
        Color::Red
    };

    // Format tokens as compact strings (e.g., "38.5k" instead of "38531")
    let format_tokens = |tokens: u32| -> String {
        if tokens >= 1_000_000 {
            format!("{:.1}m", tokens as f64 / 1_000_000.0)
        } else if tokens >= 1_000 {
            let k = tokens as f64 / 1000.0;
            if k >= 100.0 {
                format!("{:.0}k", k)
            } else {
                format!("{:.1}k", k)
            }
        } else {
            format!("{}", tokens)
        }
    };

    // Print with colored dots (using print! directly to handle color codes)
    print!(
        "{}{}{}{} {}/{} ◉ | {:.0}%\n",
        SetForegroundColor(color),
        filled_str,
        empty_str,
        ResetColor,
        format_tokens(used_tokens),
        format_tokens(total_tokens),
        percentage
    );
}

/// Set up the workspace directory for autonomous mode.
/// Uses G3_WORKSPACE environment variable or defaults to ~/tmp/workspace.
pub fn setup_workspace_directory() -> Result<PathBuf> {
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

/// Load configuration with CLI argument overrides applied.
///
/// This is the canonical function for loading config with CLI overrides.
/// All CLI entry points should use this to ensure consistent behavior.
pub fn load_config_with_cli_overrides(cli: &Cli) -> Result<Config> {
    let mut config = Config::load_with_overrides(
        cli.config.as_deref(),
        cli.provider.clone(),
        cli.model.clone(),
    )?;

    // Apply the fallback-model override.
    //
    // Applied AFTER load_with_overrides so that `--model X --fallback-model=X`
    // is detected against the *effective* default model (the one --model just
    // installed), not the one in the config file. Registration treats
    // fallback == default as "no fallback", so this is what makes that check
    // meaningful.
    if let Some(fallback) = cli.fallback_model.clone() {
        config.providers.fallback_model = Some(fallback);
    }

    // Apply thinning-floor override, if passed (see cli_args.rs doc comment).
    if let Some(floor) = cli.thinning_floor {
        config.agent.thinning_floor_percent = floor;
    }

    // Apply webdriver flag override
    if cli.webdriver {
        config.webdriver.enabled = true;
    }

    // Apply chrome-headless flag override
    // Only apply chrome-headless if safari is not explicitly set
    if cli.chrome_headless && !cli.safari {
        config.webdriver.enabled = true;
        config.webdriver.browser = g3_config::WebDriverBrowser::ChromeHeadless;

        // Run Chrome diagnostics - only show output if there are issues
        let report =
            g3_computer_control::run_chrome_diagnostics(config.webdriver.chrome_binary.as_deref());
        if !report.all_ok() {
            println!("{}", report.format_report());
        }
    }

    // Apply safari flag override
    if cli.safari {
        config.webdriver.enabled = true;
        config.webdriver.browser = g3_config::WebDriverBrowser::Safari;
    }

    // Apply no-auto-compact flag override
    if cli.manual_compact {
        config.agent.auto_compact = false;
    }

    // Validate provider if specified
    if let Some(ref provider) = cli.provider {
        let valid_providers = ["anthropic", "databricks", "embedded", "gemini", "openai"];
        let provider_type = provider.split('.').next().unwrap_or(provider);
        if !valid_providers.contains(&provider_type) {
            return Err(anyhow::anyhow!(
                "Invalid provider '{}'. Provider type must be one of: {:?}",
                provider,
                valid_providers
            ));
        }
    }

    Ok(config)
}

/// Initialize logging based on CLI verbosity settings.
pub fn initialize_logging(verbose: bool) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter = if verbose {
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
            .add_directive("llama_cpp=off".parse().unwrap())
            .add_directive("llama=off".parse().unwrap())
    };

    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .try_init();
}
