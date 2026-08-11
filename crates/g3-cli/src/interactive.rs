//! Interactive mode for G3 CLI.

use anyhow::Result;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use rustyline::error::ReadlineError;
use rustyline::{Cmd, Config, Editor, EventHandler, KeyCode, KeyEvent, Modifiers};
use crate::completion::G3Helper;
use std::path::Path;
use tracing::{debug, error};

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;

use crate::commands::{handle_command, CommandResult};
use crate::display::{LoadedContent, print_loaded_status, print_workspace_path};
use crate::g3_status::G3Status;
use crate::project::Project;
use crate::simple_output::SimpleOutput;
use crate::input_formatter::reprint_formatted_input;
use crate::template::process_template;
use crate::task_execution::execute_task_with_retry;

/// Plan mode prompt string.
const PLAN_MODE_PROMPT: &str = " [plan mode] >> ";

/// Build the interactive prompt string.
///
/// Format:
/// - Multiline mode: `"... > "`
/// - Plan mode: `" >> "`
/// - No project: `"agent_name> "` (defaults to "g3")
/// - With project: `"agent_name | project_name> "`
pub fn build_prompt(in_multiline: bool, in_plan_mode: bool, agent_name: Option<&str>, active_project: &Option<Project>) -> String {
    if in_multiline {
        "... > ".to_string()
    } else if in_plan_mode {
        PLAN_MODE_PROMPT.to_string()
    } else {
        let base_name = agent_name.unwrap_or("g3");
        if let Some(project) = active_project {
            let project_name = project.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project");
            format!("{} | {}> ", base_name, project_name)
        } else {
            format!("{}> ", base_name)
        }
    }
}

/// Prepare user input for plan mode, prepending "Create a plan: " if this is the first message.
/// Returns the (possibly modified) input and whether the flag should be reset.
pub fn prepare_plan_mode_input(input: &str, is_first_plan_message: bool, in_plan_mode: bool) -> (String, bool) {
    if in_plan_mode && is_first_plan_message {
        // Prepend "Create a plan: " and signal to reset the flag
        (format!("Create a plan: {}", input), true)
    } else {
        // No modification needed
        (input.to_string(), false)
    }
}

/// Execute user input with template processing and auto-memory reminder.
///
/// This is the common path for both single-line and multiline input.
async fn execute_user_input<W: UiWriter>(
    agent: &mut Agent<W>,
    input: &str,
    show_prompt: bool,
    show_code: bool,
    output: &SimpleOutput,
    skip_auto_memory: bool,
) {
    let processed_input = process_template(input);
    execute_task_with_retry(agent, &processed_input, show_prompt, show_code, output).await;

    // Send auto-memory reminder if enabled and tools were called
    if !skip_auto_memory {
        if let Err(e) = agent.send_auto_memory_reminder().await {
            debug!("Auto-memory reminder failed: {}", e);
        }
    }
}

/// Check if plan is terminal and exit plan mode if so.
///
/// Returns true if plan mode was exited (plan is complete or all blocked).
fn check_and_exit_plan_mode_if_terminal<W: UiWriter>(
    agent: &mut Agent<W>,
    in_plan_mode: &mut bool,
    output: &SimpleOutput,
) -> bool {
    if *in_plan_mode && agent.is_plan_terminal() {
        output.print("\n📋 Plan complete - exiting plan mode");
        *in_plan_mode = false;
        agent.set_plan_mode(false, None);
        return true;
    }
    false
}

/// Run interactive mode with console output.
/// If `agent_name` is Some, we're in agent+chat mode: skip session resume/verbose welcome,
/// and use the agent name as the prompt (e.g., "butler>").
/// If `initial_project` is Some, the project is pre-loaded (from --project flag).
pub async fn run_interactive<W: UiWriter>(
    mut agent: Agent<W>,
    show_prompt: bool,
    show_code: bool,
    combined_content: Option<String>,
    workspace_path: &Path,
    agent_name: Option<&str>,
    initial_project: Option<Project>,
) -> Result<()> {
    let output = SimpleOutput::new();
    let from_agent_mode = agent_name.is_some();

    // Skip verbose welcome when coming from agent mode (it already printed context info)
    if !from_agent_mode {
        match agent.get_provider_info() {
            Ok((provider, model)) => {
                print!(
                    "🔧 {}{}{} | {}{}{}\n",
                    SetForegroundColor(Color::Cyan),
                    provider,
                    ResetColor,
                    SetForegroundColor(Color::Yellow),
                    model,
                    ResetColor
                );
            }
            Err(e) => {
                error!("Failed to get provider info: {}", e);
            }
        }

           // Display message if AGENTS.md or README was loaded
        if let Some(ref content) = combined_content {
            let loaded = LoadedContent::from_combined_content(content);

            print_loaded_status(&loaded);
        }

        // Display workspace path
        print_workspace_path(workspace_path);
        
        // Print welcome message right before the prompt
        output.print("");
        output.print("g3 programming agent");
        output.print("   what shall we build today?");
    }
    
    // Track plan mode state (start in plan mode for non-agent mode)
    let mut in_plan_mode = !from_agent_mode;
    
    // Track if this is the first message in plan mode (to prepend "Create a plan: ")
    let mut is_first_plan_message = in_plan_mode;
    
    // Sync agent's plan mode state with CLI state
    agent.set_plan_mode(in_plan_mode, Some(workspace_path.to_str().unwrap_or(".")));

    // Initialize rustyline editor with history
    let config = Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(G3Helper::new()));

    // Bind Alt+Enter to insert a newline (for multi-line input)
    // Note: Shift+Enter is not distinguishable in standard terminals
    rl.bind_sequence(KeyEvent(KeyCode::Enter, Modifiers::ALT), EventHandler::Simple(Cmd::Newline));

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

    // Track active project (may be pre-loaded from --project flag)
    let mut active_project: Option<Project> = initial_project;

    // If we have an initial project, display its status
    if let Some(ref project) = active_project {
        let project_name = project.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project");
        G3Status::loading_project(project_name, &project.format_loaded_status());
        
        // Print newline after the loading message (G3Status::loading_project doesn't add one)
        use std::io::Write;
        println!();
        std::io::stdout().flush().ok();
    }

    loop {
        // Display context window progress bar before each prompt
        // Route through the UI writer so EventStreamWriter tees an NDJSON
        // context_summary event for callers like butler.app.
        {
            let cw = agent.get_context_window();
            agent.ui_writer().print_context_summary(
                cw.used_tokens,
                cw.total_tokens,
                cw.percentage_used(),
            );
        }

        // Build prompt
        let prompt = build_prompt(in_multiline, in_plan_mode, agent_name, &active_project);

        let readline = rl.readline(&prompt);
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

                    // Reprint input with formatting
                    reprint_formatted_input(&input, &prompt);

                    // Prepend "Create a plan: " for first message in plan mode
                    let (final_input, should_reset) = prepare_plan_mode_input(&input, is_first_plan_message, in_plan_mode);
                    if should_reset {
                        is_first_plan_message = false;
                    }
                    execute_user_input(
                        &mut agent, &final_input, show_prompt, show_code, &output, from_agent_mode
                    ).await;

                    // Check if plan completed and exit plan mode if so
                    check_and_exit_plan_mode_if_terminal(&mut agent, &mut in_plan_mode, &output);
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
                        let result = handle_command(&input, &mut agent, workspace_path, &output, &mut active_project, &mut rl, show_prompt, show_code).await?;
                        
                        match result {
                            CommandResult::Handled => {
                                continue;
                            }
                            CommandResult::EnterPlanMode => {
                                in_plan_mode = true;
                                agent.set_plan_mode(true, Some(workspace_path.to_str().unwrap_or(".")));
                                is_first_plan_message = true;
                                continue;
                            }
                        }
                    }

                    // Reprint input with formatting
                    reprint_formatted_input(&input, &prompt);

                    // Prepend "Create a plan: " for first message in plan mode
                    let (final_input, should_reset) = prepare_plan_mode_input(&input, is_first_plan_message, in_plan_mode);
                    if should_reset {
                        is_first_plan_message = false;
                    }
                    execute_user_input(
                        &mut agent, &final_input, show_prompt, show_code, &output, from_agent_mode
                    ).await;

                    // Check if plan completed and exit plan mode if so
                    check_and_exit_plan_mode_if_terminal(&mut agent, &mut in_plan_mode, &output);
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
                // CTRL-D: if in plan mode, exit plan mode first; otherwise exit g3
                if in_plan_mode {
                    output.print("CTRL-D (exiting plan mode)");
                    in_plan_mode = false;
                    agent.set_plan_mode(false, None);
                    // Continue the loop with normal prompt
                    continue;
                } else {
                    output.print("CTRL-D");
                    break;
                }
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

    // Save session continuation for resume capability
    agent.save_session_continuation(None);

    // Send auto-memory reminder once on exit when in agent+chat mode
    // (Per-turn reminders were skipped to avoid being too onerous)
    if from_agent_mode {
        if let Err(e) = agent.send_auto_memory_reminder().await {
            debug!("Auto-memory reminder on exit failed: {}", e);
        }
    }

    output.print("👋 Goodbye!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_test_project(name: &str) -> Project {
        Project {
            path: PathBuf::from(format!("/test/projects/{}", name)),
            content: "test content".to_string(),
            loaded_files: vec!["brief.md".to_string()],
        }
    }

    #[test]
    fn test_build_prompt_default() {
        let prompt = build_prompt(false, false, None, &None);
        assert_eq!(prompt, "g3> ");
    }

    #[test]
    fn test_build_prompt_with_agent_name() {
        let prompt = build_prompt(false, false, Some("butler"), &None);
        assert_eq!(prompt, "butler> ");
    }

    #[test]
    fn test_build_prompt_multiline() {
        let prompt = build_prompt(true, false, None, &None);
        assert_eq!(prompt, "... > ");

        // Multiline takes precedence over agent name
        let prompt = build_prompt(true, false, Some("butler"), &None);
        assert_eq!(prompt, "... > ");

        // Multiline takes precedence over project
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(true, false, None, &project);
        assert_eq!(prompt, "... > ");
        
        // Multiline takes precedence over plan mode
        let prompt = build_prompt(true, true, None, &None);
        assert_eq!(prompt, "... > ");
    }

    #[test]
    fn test_build_prompt_plan_mode() {
        let prompt = build_prompt(false, true, None, &None);
        assert_eq!(prompt, " [plan mode] >> ");
        
        // Plan mode takes precedence over agent name
        let prompt = build_prompt(false, true, Some("butler"), &None);
        assert_eq!(prompt, " [plan mode] >> ");
        
        // Plan mode takes precedence over project
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, true, None, &project);
        assert_eq!(prompt, " [plan mode] >> ");
    }

    #[test]
    fn test_build_prompt_with_project() {
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, false, None, &project);
        assert!(prompt.contains("g3"));
        assert!(prompt.contains("myapp"));
        assert!(prompt.contains("|"));
    }

    #[test]
    fn test_build_prompt_with_agent_and_project() {
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, false, Some("carmack"), &project);
        assert!(prompt.contains("carmack"));
        assert!(prompt.contains("myapp"));
        assert!(prompt.contains("|"));
    }

    #[test]
    fn test_build_prompt_unproject_resets() {
        // Simulate /project loading
        let project = Some(create_test_project("myapp"));
        let prompt_with_project = build_prompt(false, false, None, &project);
        assert!(prompt_with_project.contains("myapp"));

        // Simulate /unproject (sets active_project to None)
        let prompt_after_unproject = build_prompt(false, false, None, &None);
        assert_eq!(prompt_after_unproject, "g3> ");
        assert!(!prompt_after_unproject.contains("myapp"));
    }

    #[test]
    fn test_build_prompt_project_name_from_path() {
        let project = Some(Project {
            path: PathBuf::from("/Users/dev/projects/awesome-app"),
            content: "test".to_string(),
            loaded_files: vec![],
        });
        let prompt = build_prompt(false, false, None, &project);
        assert!(prompt.contains("awesome-app"));
    }

    // Tests for prepare_plan_mode_input

    #[test]
    fn test_prepare_plan_mode_input_happy_path_first_message() {
        // Happy path: First message in plan mode gets "Create a plan: " prefix
        let (result, should_reset) = prepare_plan_mode_input("fix the bug", true, true);
        assert_eq!(result, "Create a plan: fix the bug");
        assert!(should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_negative_second_message() {
        // Negative: Second message (is_first_plan_message = false) should NOT get prefix
        let (result, should_reset) = prepare_plan_mode_input("fix the bug", false, true);
        assert_eq!(result, "fix the bug");
        assert!(!should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_negative_not_in_plan_mode() {
        // Negative: Not in plan mode should NOT get prefix even if is_first_plan_message is true
        let (result, should_reset) = prepare_plan_mode_input("fix the bug", true, false);
        assert_eq!(result, "fix the bug");
        assert!(!should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_negative_neither_condition() {
        // Negative: Neither in plan mode nor first message
        let (result, should_reset) = prepare_plan_mode_input("fix the bug", false, false);
        assert_eq!(result, "fix the bug");
        assert!(!should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_boundary_empty_input() {
        // Boundary: Empty input would get prefix, but in practice empty input
        // is filtered out by the caller before reaching this function.
        // This test documents the function's behavior in isolation.
        let (result, should_reset) = prepare_plan_mode_input("", true, true);
        assert_eq!(result, "Create a plan: ");
        assert!(should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_boundary_whitespace_input() {
        // Boundary: Whitespace-only input gets prefix preserved
        let (result, should_reset) = prepare_plan_mode_input("   ", true, true);
        assert_eq!(result, "Create a plan:    ");
        assert!(should_reset);
    }

    #[test]
    fn test_prepare_plan_mode_input_boundary_multiline_input() {
        // Boundary: Multiline input gets prefix on first line only
        let (result, should_reset) = prepare_plan_mode_input("line1\nline2\nline3", true, true);
        assert_eq!(result, "Create a plan: line1\nline2\nline3");
        assert!(should_reset);
    }
}

