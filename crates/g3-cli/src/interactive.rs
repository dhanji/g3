//! Interactive mode for G3 CLI.

use anyhow::Result;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use rustyline::error::ReadlineError;
use rustyline::{Config, Editor};
use crate::completion::G3Helper;
use std::path::Path;
use tracing::{debug, error};

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;

use crate::commands::handle_command;
use crate::display::{LoadedContent, print_loaded_status, print_project_heading, print_workspace_path};
use crate::g3_status::{G3Status, Status};
use crate::project::Project;
use crate::project_files::extract_readme_heading;
use crate::simple_output::SimpleOutput;
use crate::template::process_template;
use crate::task_execution::execute_task_with_retry;
use crate::utils::display_context_progress;

/// Build the interactive prompt string.
///
/// Format:
/// - Multiline mode: `"... > "`
/// - No project: `"agent_name> "` (defaults to "g3")
/// - With project: `"agent_name | project_name> "`
pub fn build_prompt(in_multiline: bool, agent_name: Option<&str>, active_project: &Option<Project>) -> String {
    if in_multiline {
        "... > ".to_string()
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

/// Run interactive mode with console output.
/// If `agent_name` is Some, we're in agent+chat mode: skip session resume/verbose welcome,
/// and use the agent name as the prompt (e.g., "butler>").
pub async fn run_interactive<W: UiWriter>(
    mut agent: Agent<W>,
    show_prompt: bool,
    show_code: bool,
    combined_content: Option<String>,
    workspace_path: &Path,
    new_session: bool,
    agent_name: Option<&str>,
) -> Result<()> {
    let output = SimpleOutput::new();
    let from_agent_mode = agent_name.is_some();

    // Check for session continuation (skip if --new-session was passed or coming from agent mode)
    // Agent mode with --chat should start fresh without prompting
    if !new_session && !from_agent_mode {
      if let Ok(Some(continuation)) = g3_core::load_continuation() {
        // Truncate session_id to 20 characters, handling UTF-8 properly
        let session_display: String = continuation.session_id.chars().take(20).collect();
        // Print session info and prompt on same line (no newline)
        print!(
            "\n >> session in progress: {}{}{} | {:.1}% used | resume? [y/n] ",
            SetForegroundColor(Color::Cyan),
            session_display,
            ResetColor,
            continuation.context_percentage
        );
        use std::io::Write;
        std::io::stdout().flush()?;

        // Read user input
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input.is_empty() || input == "y" || input == "yes" {
            // Resume the session
            match agent.restore_from_continuation(&continuation) {
                Ok(true) => {
                    G3Status::resuming(&continuation.session_id, Status::Done);
                }
                Ok(false) => {
                    G3Status::resuming_summary(&continuation.session_id);
                }
                Err(e) => {
                    G3Status::resuming(&continuation.session_id, Status::Error(e.to_string()));
                    // Clear the invalid continuation
                    let _ = g3_core::clear_continuation();
                }
            }
        } else {
            // User declined, clear the continuation
            G3Status::info_inline("starting fresh");
            let _ = g3_core::clear_continuation();
        }
      }
    }

    // Skip verbose welcome when coming from agent mode (it already printed context info)
    if !from_agent_mode {
        output.print("");
        output.print("g3 programming agent");
        output.print("      >> what shall we build today?");
        output.print("");

        // Display provider and model information
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

            // Extract project name if README is loaded
            if loaded.has_readme {
                if let Some(name) = extract_readme_heading(content) {
                    print_project_heading(&name);
                }
            }

            print_loaded_status(&loaded);
        }

        // Display workspace path
        print_workspace_path(workspace_path);
        output.print("");
    }

    // Initialize rustyline editor with history
    let config = Config::builder()
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl = Editor::with_config(config)?;
    rl.set_helper(Some(G3Helper::new()));

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

    // Track active project
    let mut active_project: Option<Project> = None;

    loop {
        // Display context window progress bar before each prompt
        display_context_progress(&agent, &output);

        // Build prompt
        let prompt = build_prompt(in_multiline, agent_name, &active_project);

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

                    // Process the multiline input (with template expansion)
                    let processed_input = process_template(&input);
                    execute_task_with_retry(
                        &mut agent,
                        &processed_input,
                        show_prompt,
                        show_code,
                        &output,
                    )
                    .await;

                    // Send auto-memory reminder if enabled and tools were called
                    // Skip per-turn reminders when from_agent_mode - we'll send once on exit
                    if !from_agent_mode {
                      if let Err(e) = agent.send_auto_memory_reminder().await {
                        debug!("Auto-memory reminder failed: {}", e);
                      }
                    }
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
                        if handle_command(&input, &mut agent, workspace_path, &output, &mut active_project, &mut rl, show_prompt, show_code).await? {
                            continue;
                        }
                    }

                    // Process the single line input (with template expansion)
                    let processed_input = process_template(&input);
                    execute_task_with_retry(
                        &mut agent,
                        &processed_input,
                        show_prompt,
                        show_code,
                        &output,
                    )
                    .await;

                    // Send auto-memory reminder if enabled and tools were called
                    // Skip per-turn reminders when from_agent_mode - we'll send once on exit
                    if !from_agent_mode {
                      if let Err(e) = agent.send_auto_memory_reminder().await {
                        debug!("Auto-memory reminder failed: {}", e);
                      }
                    }
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
        let prompt = build_prompt(false, None, &None);
        assert_eq!(prompt, "g3> ");
    }

    #[test]
    fn test_build_prompt_with_agent_name() {
        let prompt = build_prompt(false, Some("butler"), &None);
        assert_eq!(prompt, "butler> ");
    }

    #[test]
    fn test_build_prompt_multiline() {
        let prompt = build_prompt(true, None, &None);
        assert_eq!(prompt, "... > ");

        // Multiline takes precedence over agent name
        let prompt = build_prompt(true, Some("butler"), &None);
        assert_eq!(prompt, "... > ");

        // Multiline takes precedence over project
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(true, None, &project);
        assert_eq!(prompt, "... > ");
    }

    #[test]
    fn test_build_prompt_with_project() {
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, None, &project);
        // Should contain the project name in the prompt
        assert!(prompt.contains("g3"));
        assert!(prompt.contains("myapp"));
        assert!(prompt.contains("|"));
    }

    #[test]
    fn test_build_prompt_with_agent_and_project() {
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, Some("carmack"), &project);
        // Should contain both agent name and project name
        assert!(prompt.contains("carmack"));
        assert!(prompt.contains("myapp"));
        assert!(prompt.contains("|"));
    }

    #[test]
    fn test_build_prompt_unproject_resets() {
        // Simulate /project loading
        let project = Some(create_test_project("myapp"));
        let prompt_with_project = build_prompt(false, None, &project);
        assert!(prompt_with_project.contains("myapp"));

        // Simulate /unproject (sets active_project to None)
        let prompt_after_unproject = build_prompt(false, None, &None);
        assert_eq!(prompt_after_unproject, "g3> ");
        assert!(!prompt_after_unproject.contains("myapp"));
    }

    #[test]
    fn test_build_prompt_project_name_from_path() {
        // Test that project name is extracted from path
        let project = Some(Project {
            path: PathBuf::from("/Users/dev/projects/awesome-app"),
            content: "test".to_string(),
            loaded_files: vec![],
        });
        let prompt = build_prompt(false, None, &project);
        assert!(prompt.contains("awesome-app"));
    }
}

