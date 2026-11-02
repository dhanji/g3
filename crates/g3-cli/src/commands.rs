//! Interactive command handlers for G3 CLI.
//!
//! Handles `/` commands in interactive mode (help, compact, etc.).

use anyhow::Result;
use rustyline::Editor;

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;

use crate::completion::G3Helper;
use crate::g3_status::{G3Status, Status};
use crate::simple_output::SimpleOutput;
use crate::project::Project;
use crate::project::load_and_validate_project;
use crate::template::process_template;
use crate::task_execution::execute_task_with_retry;

/// Result of handling a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// Command was handled, continue the loop
    Handled,
    /// Enter plan mode (after /plan command)
    EnterPlanMode,
}

/// Handle a control command. Returns true if the command was handled and the loop should continue.
pub async fn handle_command<W: UiWriter>(
    input: &str,
    agent: &mut Agent<W>,
    workspace_dir: &std::path::Path,
    output: &SimpleOutput,
    active_project: &mut Option<Project>,
    rl: &mut Editor<G3Helper, rustyline::history::DefaultHistory>,
    show_prompt: bool,
    show_code: bool,
) -> Result<CommandResult> {
    match input {
        "/help" => {
            output.print("");
            output.print("📖 Control Commands:");
            output.print("  /compact   - Trigger compaction (compacts conversation history)");
            output.print("  /thinnify  - Trigger context thinning (replaces large tool results with file references)");
            output.print("  /skinnify  - Trigger full context thinning (like /thinnify but for entire context, not just first third)");
            output.print("  /clear     - Clear session and start fresh (discards continuation artifacts)");
            output.print("  /resume    - List and switch to a previous session");
            output.print("  /project <path> - Load a project from the given absolute path");
            output.print("  /unproject - Unload the current project and reset context");
            output.print("  /dump      - Dump entire context window to file for debugging");
            output.print("  /readme    - Reload README.md and AGENTS.md from disk");
            output.print("  /stats     - Show detailed context and performance statistics");
            output.print("  /run <file> - Read file and execute as prompt");
            output.print("  /plan <description> - Start Plan Mode for a new feature");
            output.print("  /help      - Show this help message");
            output.print("  exit/quit  - Exit the interactive session");
            output.print("");
            Ok(CommandResult::Handled)
        }
        "/compact" => {
            output.print_g3_progress("compacting session");
            match agent.force_compact().await {
                Ok(true) => {
                    output.print_g3_status("compacting session", "done");
                }
                Ok(false) => {
                    output.print_g3_status("compacting session", "failed");
                }
                Err(e) => {
                    output.print_g3_status("compacting session", &format!("error: {}", e));
                }
            }
            Ok(CommandResult::Handled)
        }
        "/thinnify" => {
            let result = agent.force_thin();
            G3Status::thin_result(&result);
            Ok(CommandResult::Handled)
        }
        "/skinnify" => {
            let result = agent.force_thin_all();
            G3Status::thin_result(&result);
            Ok(CommandResult::Handled)
        }
        cmd if cmd.starts_with("/run") => {
            let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                output.print("Usage: /run <file-path>");
                output.print("Reads the file and executes its content as a prompt.");
            } else {
                let file_path = parts[1].trim();
                // Expand tilde
                let expanded_path = if file_path.starts_with("~/") {
                    if let Some(home) = dirs::home_dir() {
                        home.join(&file_path[2..])
                    } else {
                        std::path::PathBuf::from(file_path)
                    }
                } else {
                    std::path::PathBuf::from(file_path)
                };
                match std::fs::read_to_string(&expanded_path) {
                    Ok(content) => {
                        let processed = process_template(&content);
                        let prompt = processed.trim();
                        if prompt.is_empty() {
                            output.print("❌ File is empty.");
                        } else {
                            G3Status::progress(&format!("loading {}", file_path));
                            G3Status::done();
                            execute_task_with_retry(agent, prompt, show_prompt, show_code, output).await;
                        }
                    }
                    Err(e) => {
                        output.print(&format!("❌ Failed to read file '{}': {}", file_path, e));
                    }
                }
            }
            Ok(CommandResult::Handled)
        }
        "/dump" => {
            // Dump entire context window to a file for debugging
            let dump_dir = std::path::Path::new("tmp");
            if !dump_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(dump_dir) {
                    output.print(&format!("❌ Failed to create tmp directory: {}", e));
                    return Ok(CommandResult::Handled);
                }
            }

            let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let dump_path = dump_dir.join(format!("context_dump_{}.txt", timestamp));

            let context = agent.get_context_window();
            let mut dump_content = String::new();
            dump_content.push_str("# Context Window Dump\n");
            dump_content.push_str(&format!("# Timestamp: {}\n", chrono::Utc::now()));
            dump_content.push_str(&format!(
                "# Messages: {}\n",
                context.conversation_history.len()
            ));
            dump_content.push_str(&format!(
                "# Used tokens: {} / {} ({:.1}%)\n\n",
                context.used_tokens,
                context.total_tokens,
                context.percentage_used()
            ));

            for (i, msg) in context.conversation_history.iter().enumerate() {
                dump_content.push_str(&format!("=== Message {} ===\n", i));
                dump_content.push_str(&format!("Role: {:?}\n", msg.role));
                dump_content.push_str(&format!("Kind: {:?}\n", msg.kind));
                dump_content.push_str(&format!("Content ({} chars):\n", msg.content.len()));
                dump_content.push_str(&msg.content);
                dump_content.push_str("\n\n");
            }

            match std::fs::write(&dump_path, &dump_content) {
                Ok(_) => {
                    G3Status::complete_with_path(
                        "context dumped to",
                        &dump_path.display().to_string(),
                        Status::Done,
                    );
                }
                Err(e) => output.print(&format!("❌ Failed to write dump: {}", e)),
            }
            Ok(CommandResult::Handled)
        }
        "/clear" => {
            use crate::g3_status::G3Status;
            G3Status::progress("clearing session");
            agent.clear_session();
            G3Status::done();
            output.print("Starting fresh.");
            Ok(CommandResult::Handled)
        }
        "/readme" => {
            use crate::g3_status::G3Status;
            G3Status::progress("reloading README");
            match agent.reload_readme() {
                Ok(true) => {
                    G3Status::done();
                }
                Ok(false) => {
                    G3Status::failed();
                    output.print("No README was loaded at startup, cannot reload");
                }
                Err(e) => {
                    G3Status::error(&e.to_string());
                }
            }
            Ok(CommandResult::Handled)
        }
        "/stats" => {
            let stats = agent.get_stats();
            output.print(&stats);
            Ok(CommandResult::Handled)
        }
        "/resume" => {
            output.print("📋 Scanning for available sessions...");

            match g3_core::list_sessions_for_directory() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        output.print("No sessions found for this directory.");
                        return Ok(CommandResult::Handled);
                    }

                    // Get current session ID to mark it
                    let current_session_id = agent.get_session_id().map(|s| s.to_string());

                    output.print("");
                    output.print("Available sessions:");
                    for (i, session) in sessions.iter().enumerate() {
                        let time_str = g3_core::format_session_time(&session.created_at);
                        let context_str = format!("{:.0}%", session.context_percentage);
                        let current_marker =
                            if current_session_id.as_deref() == Some(&session.session_id) {
                                " (current)"
                            } else {
                                ""
                            };
                        let todo_marker = if session.has_incomplete_todos() {
                            " 📝"
                        } else {
                            ""
                        };

                        // Use description if available, otherwise fall back to session ID
                        let display_name = match &session.description {
                            Some(desc) => format!("'{}'", desc),
                            None => {
                                if session.session_id.len() > 40 {
                                    format!("{}...", &session.session_id[..40])
                                } else {
                                    session.session_id.clone()
                                }
                            }
                        };
                        output.print(&format!(
                            "  {}. [{}] {} ({}){}{}\n",
                            i + 1,
                            time_str,
                            display_name,
                            context_str,
                            todo_marker,
                            current_marker
                        ));
                    }

                    output.print_inline("\nSession number to resume (Enter to cancel): ");
                    // Read user selection
                    if let Ok(selection) = rl.readline("") {
                        let selection = selection.trim();
                        if selection.is_empty() {
                            output.print("Cancelled.");
                        } else if let Ok(num) = selection.parse::<usize>() {
                            if num >= 1 && num <= sessions.len() {
                                let selected = &sessions[num - 1];
                                match agent.switch_to_session(selected) {
                                    Ok(true) => {
                                        G3Status::resuming(&selected.session_id, Status::Done);
                                    }
                                    Ok(false) => {
                                        G3Status::resuming_summary(&selected.session_id);
                                    }
                                    Err(e) => {
                                        G3Status::resuming(&selected.session_id, Status::Error(e.to_string()));
                                    }
                                }
                            } else {
                                output.print("Invalid selection.");
                            }
                        } else {
                            output.print("Invalid input. Please enter a number.");
                        }
                    }
                }
                Err(e) => output.print(&format!("❌ Error listing sessions: {}", e)),
            }
            Ok(CommandResult::Handled)
        }
        cmd if cmd.starts_with("/project") => {
            let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                output.print("Usage: /project <absolute-path>");
                output.print("Loads project files (brief.md, contacts.yaml, status.md) from the given path.");
            } else {
                let project_path_str = parts[1].trim();

                // Use shared helper for validation and loading
                match load_and_validate_project(project_path_str, workspace_dir) {
                    Ok(project) => {
                        // Set project content in agent's system message
                        if agent.set_project_content(Some(project.content.clone())) {
                            // Set project path on UI writer for path shortening
                            let project_name = project.path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("project")
                                .to_string();
                            agent.ui_writer().set_project_path(project.path.clone(), project_name);

                            // Print loaded status
                            let project_name = project.path.file_name()
                                .and_then(|n| n.to_str()).unwrap_or("project");
                            G3Status::loading_project(project_name, &project.format_loaded_status());

                            // Store active project
                            *active_project = Some(project);
                        } else {
                            output.print("❌ Failed to set project content in agent context.");
                        }
                    }
                    Err(e) => {
                        output.print(&format!("❌ {}", e));
                    }
                }
            }
            Ok(CommandResult::Handled)
        }
        cmd if cmd.starts_with("/plan") => {
            let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                output.print("Usage: /plan <description>");
                output.print("Starts Plan Mode for a new feature. The agent will:");
                output.print("  1. Research and draft a Plan with checks (happy/negative/boundary)");
                output.print("  2. Ask clarifying questions if needed");
                output.print("  3. Request approval before coding");
                output.print("");
                output.print("Example: /plan Add CSV import for comic book metadata");
                Ok(CommandResult::Handled)
            } else {
                let feature_description = parts[1].trim();
                
                // Construct the feature prompt that instructs the agent to use Plan Mode
                let prompt = format!(
                    "I want to implement a new feature: {}\n\n\
                    Please use Plan Mode to help me implement this:\n\
                    1. First, research the codebase to understand where this feature should live\n\
                    2. Draft a Plan using `plan_write` with items that have all three checks (happy, negative, boundary)\n\
                    3. Ask me any clarifying questions if needed\n\
                    4. Then ask me to approve the plan before you start coding\n\n\
                    Do NOT start coding until I approve the plan.",
                    feature_description
                );
                
                // Print the welcome message for plan mode
                output.print("   what shall we build today?");
                
                execute_task_with_retry(agent, &prompt, show_prompt, show_code, output).await;
                
                // Return EnterPlanMode to signal interactive loop to switch prompts
                Ok(CommandResult::EnterPlanMode)
            }
        }
        "/unproject" => {
            if active_project.is_some() {
                use crate::g3_status::G3Status;
                G3Status::progress("unloading project");
                agent.clear_project_content();
                agent.ui_writer().clear_project();
                *active_project = None;
                G3Status::done();
                output.print("Context reset to original system message.");
            } else {
                output.print("No project is currently loaded.");
            }
            Ok(CommandResult::Handled)
        }
        _ => {
            output.print(&format!(
                "❌ Unknown command: {}. Type /help for available commands.",
                input
            ));
            Ok(CommandResult::Handled)
        }
    }
}
