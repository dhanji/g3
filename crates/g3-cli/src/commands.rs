//! Interactive command handlers for G3 CLI.
//!
//! Handles `/` commands in interactive mode.

use anyhow::Result;
use rustyline::Editor;
use std::path::PathBuf;

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;

use crate::completion::G3Helper;
use crate::g3_status::{G3Status, Status};
use crate::simple_output::SimpleOutput;
use crate::project::Project;
use crate::template::process_template;
use crate::task_execution::execute_task_with_retry;

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
) -> Result<bool> {
    match input {
        "/help" => {
            output.print("");
            output.print("📖 Control Commands:");
            output.print("  /compact   - Trigger compaction (compacts conversation history)");
            output.print("  /thinnify  - Trigger context thinning (replaces large tool results with file references)");
            output.print("  /skinnify  - Trigger full context thinning (like /thinnify but for entire context, not just first third)");
            output.print("  /clear     - Clear session and start fresh (discards continuation artifacts)");
            output.print("  /fragments - List dehydrated context fragments (ACD)");
            output.print("  /rehydrate - Restore a dehydrated fragment by ID");
            output.print("  /resume    - List and switch to a previous session");
            output.print("  /project <path> - Load a project from the given absolute path");
            output.print("  /unproject - Unload the current project and reset context");
            output.print("  /dump      - Dump entire context window to file for debugging");
            output.print("  /readme    - Reload README.md and AGENTS.md from disk");
            output.print("  /stats     - Show detailed context and performance statistics");
            output.print("  /run <file> - Read file and execute as prompt");
            output.print("  /help      - Show this help message");
            output.print("  exit/quit  - Exit the interactive session");
            output.print("");
            Ok(true)
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
            Ok(true)
        }
        "/thinnify" => {
            let result = agent.force_thin();
            G3Status::thin_result(&result);
            Ok(true)
        }
        "/skinnify" => {
            let result = agent.force_thin_all();
            G3Status::thin_result(&result);
            Ok(true)
        }
        "/fragments" => {
            if let Some(session_id) = agent.get_session_id() {
                match g3_core::acd::list_fragments(session_id) {
                    Ok(fragments) => {
                        if fragments.is_empty() {
                            output.print("No dehydrated fragments found for this session.");
                        } else {
                            output.print(&format!(
                                "📦 {} dehydrated fragment(s):\n",
                                fragments.len()
                            ));
                            for fragment in &fragments {
                                output.print(&fragment.generate_stub());
                                output.print("");
                            }
                        }
                    }
                    Err(e) => {
                        output.print(&format!("❌ Error listing fragments: {}", e));
                    }
                }
            } else {
                output.print("No active session - fragments are session-scoped.");
            }
            Ok(true)
        }
        cmd if cmd.starts_with("/rehydrate") => {
            let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                output.print("Usage: /rehydrate <fragment_id>");
                output.print("Use /fragments to list available fragment IDs.");
            } else {
                let fragment_id = parts[1].trim();
                if let Some(session_id) = agent.get_session_id() {
                    match g3_core::acd::Fragment::load(session_id, fragment_id) {
                        Ok(fragment) => {
                            output.print(&format!(
                                "✅ Fragment '{}' loaded ({} messages, ~{} tokens)",
                                fragment_id, fragment.message_count, fragment.estimated_tokens
                            ));
                            output.print("");
                            output.print(&fragment.generate_stub());
                        }
                        Err(e) => {
                            output.print(&format!(
                                "❌ Failed to load fragment '{}': {}",
                                fragment_id, e
                            ));
                        }
                    }
                } else {
                    output.print("No active session - fragments are session-scoped.");
                }
            }
            Ok(true)
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
                            let completed = execute_task_with_retry(agent, prompt, show_prompt, show_code, output).await;
                            if !completed {
                                return Ok(false);
                            }
                        }
                    }
                    Err(e) => {
                        output.print(&format!("❌ Failed to read file '{}': {}", file_path, e));
                    }
                }
            }
            Ok(true)
        }
        "/dump" => {
            // Dump entire context window to a file for debugging
            let dump_dir = std::path::Path::new("tmp");
            if !dump_dir.exists() {
                if let Err(e) = std::fs::create_dir_all(dump_dir) {
                    output.print(&format!("❌ Failed to create tmp directory: {}", e));
                    return Ok(true);
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
            Ok(true)
        }
        "/clear" => {
            use crate::g3_status::G3Status;
            G3Status::progress("clearing session");
            agent.clear_session();
            G3Status::done();
            output.print("Starting fresh.");
            Ok(true)
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
            Ok(true)
        }
        "/stats" => {
            let stats = agent.get_stats();
            output.print(&stats);
            Ok(true)
        }
        "/resume" => {
            output.print("📋 Scanning for available sessions...");

            match g3_core::list_sessions_for_directory() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        output.print("No sessions found for this directory.");
                        return Ok(true);
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
            Ok(true)
        }
        cmd if cmd.starts_with("/project") => {
            let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
            if parts.len() < 2 || parts[1].trim().is_empty() {
                output.print("Usage: /project <absolute-path>");
                output.print("Loads project files (brief.md, contacts.yaml, status.md) from the given path.");
            } else {
                let project_path_str = parts[1].trim();
                
                // Expand tilde if present
                let project_path = if project_path_str.starts_with("~/") {
                    if let Some(home) = dirs::home_dir() {
                        home.join(&project_path_str[2..])
                    } else {
                        PathBuf::from(project_path_str)
                    }
                } else {
                    PathBuf::from(project_path_str)
                };

                // Validate path is absolute
                if !project_path.is_absolute() {
                    output.print("❌ Project path must be absolute (e.g., /Users/name/projects/myproject)");
                    return Ok(true);
                }

                // Validate path exists
                if !project_path.exists() {
                    output.print(&format!("❌ Project path does not exist: {}", project_path.display()));
                    return Ok(true);
                }

                // Load the project
                match Project::load(&project_path, workspace_dir) {
                    Some(project) => {
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
                            
                            // Auto-submit the project status prompt
                            let prompt = "what is the current state of the project? and what is your suggested next best step?";
                            let completed = execute_task_with_retry(agent, prompt, show_prompt, show_code, output).await;
                            if !completed {
                                return Ok(false);
                            }
                        } else {
                            output.print("❌ Failed to set project content in agent context.");
                        }
                    }
                    None => {
                        output.print("❌ No project files found (brief.md, contacts.yaml, status.md).");
                    }
                }
            }
            Ok(true)
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
            Ok(true)
        }
        _ => {
            output.print(&format!(
                "❌ Unknown command: {}. Type /help for available commands.",
                input
            ));
            Ok(true)
        }
    }
}
