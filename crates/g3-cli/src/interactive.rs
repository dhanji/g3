//! Interactive mode for G3 CLI.

use anyhow::Result;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use rustyline::error::ReadlineError;
use rustyline::{Config, Editor};
use crate::completion::G3Helper;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error};
use tokio::sync::broadcast;

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;
use g3_core::ToolCall;

use crate::commands::{handle_command, CommandResult};
use crate::display::{LoadedContent, print_loaded_status, print_project_heading, print_workspace_path};
use crate::g3_status::{G3Status, Status};
use crate::project::Project;
use crate::project_files::extract_project_heading;
use crate::simple_output::SimpleOutput;
use crate::input_formatter::reprint_formatted_input;
use crate::template::process_template;
use crate::task_execution::execute_task_with_retry;
use crate::utils::display_context_progress;

/// Plan mode prompt string.
const PLAN_MODE_PROMPT: &str = " >> ";

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

/// Check if the input is an approval command (for plan mode).
///
/// Recognizes: "a", "approve", "approved", and common misspellings.
pub fn is_approval_input(input: &str) -> bool {
    let normalized = input.trim().to_lowercase();
    // Strip trailing punctuation (!, ., ,)
    let normalized = normalized.trim_end_matches(|c| c == '!' || c == '.' || c == ',');

    // Exact matches
    if matches!(normalized, "a" | "approve" | "approved" | "yes" | "y" | "ok") {
        return true;
    }
    
    // Common misspellings of "approve" / "approved"
    let misspellings = [
        "approv",      // missing 'e'
        "aprove",      // missing 'p'
        "aproved",     // missing 'p'
        "aprrove",     // transposed
        "appprove",    // extra 'p'
        "apporve",     // transposed
        "approev",     // transposed
        "approvd",     // missing 'e'
        "approed",     // missing 'v'
        "approvee",    // extra 'e'
        "approveed",   // extra 'e'
    ];
    
    misspellings.iter().any(|&m| normalized == m)
}

/// Execute plan_approve tool directly without going through the LLM.
///
/// Returns (success, message) where success indicates if the plan was approved.
async fn execute_plan_approve_directly<W: UiWriter>(
    agent: &mut Agent<W>,
    output: &SimpleOutput,
) -> (bool, String) {
    let tool_call = ToolCall {
        tool: "plan_approve".to_string(),
        args: serde_json::json!({}),
    };
    
    match agent.execute_tool_call(&tool_call).await {
        Ok(result) => {
            let success = result.contains("✅ Plan approved");
            output.print(&result);
            (success, result)
        }
        Err(e) => {
            let msg = format!("❌ Failed to approve plan: {}", e);
            output.print(&msg);
            (false, msg)
        }
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

/// Spawn a background task to handle research completion notifications.
///
/// This task listens for research completions and prints status messages in real-time.
/// When g3 is idle (waiting for input), it reprints the prompt after the notification.
/// When g3 is busy (processing), it just prints the notification (interleaving is fine).
///
/// Returns a handle to the spawned task and an `is_busy` flag that should be set
/// to true while the agent is processing and false when waiting for input.
fn spawn_research_notification_handler(
    mut rx: broadcast::Receiver<g3_core::ResearchCompletionNotification>,
    is_busy: Arc<AtomicBool>,
    prompt: Arc<std::sync::RwLock<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(notification) => {
                    use std::io::Write;
                    
                    let succeeded = notification.status == g3_core::ResearchStatus::Complete;
                    
                    // Print the completion notification
                    // If we're idle (at prompt), we need to print on a new line first
                    let busy = is_busy.load(Ordering::SeqCst);
                    if !busy {
                        // Clear the current line (prompt) and move to start
                        print!("\r\x1b[K");
                    }
                    
                    G3Status::research_complete(1, succeeded);
                    
                    // If we're idle, reprint the prompt
                    if !busy {
                        let prompt_str = prompt.read().unwrap().clone();
                        print!("{}", prompt_str);
                        let _ = std::io::stdout().flush();
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Channel closed, exit the task
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Missed some messages, continue
                    continue;
                }
            }
        }
    })
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
    new_session: bool,
    agent_name: Option<&str>,
    initial_project: Option<Project>,
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

            // Extract project name from AGENTS.md or memory
            if let Some(name) = extract_project_heading(content) {
                print_project_heading(&name);
            }

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

    // Enable research completion notifications for real-time updates
    let research_rx = agent.enable_research_notifications();
    let is_busy = Arc::new(AtomicBool::new(false));
    let current_prompt = Arc::new(std::sync::RwLock::new(String::new()));
    let _notification_handle = spawn_research_notification_handler(
        research_rx,
        is_busy.clone(),
        current_prompt.clone(),
    );

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
        display_context_progress(&agent, &output);

        // Check for completed research and inject into context
        // This happens before prompting the user for input
        let injected_count = agent.inject_completed_research();
        if injected_count > 0 {
            println!("📋 {} research result(s) ready - injected into context", injected_count);
            println!();
        }

        // Build prompt
        let prompt = build_prompt(in_multiline, in_plan_mode, agent_name, &active_project);
        
        // Update the shared prompt for the notification handler
        *current_prompt.write().unwrap() = prompt.clone();
        is_busy.store(false, Ordering::SeqCst);

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

                    is_busy.store(true, Ordering::SeqCst);
                    execute_user_input(
                        &mut agent, &input, show_prompt, show_code, &output, from_agent_mode
                    ).await;
                    is_busy.store(false, Ordering::SeqCst);
                } else {
                    // Single line input
                    let input = line.trim().to_string();

                    // In plan mode, check for approval input before anything else
                    if in_plan_mode && is_approval_input(&input) {
                        // Add to history
                        rl.add_history_entry(&input)?;
                        
                        // Reprint input with formatting
                        reprint_formatted_input(&input, &prompt);
                        
                        is_busy.store(true, Ordering::SeqCst);
                        let (approved, result) = execute_plan_approve_directly(&mut agent, &output).await;
                        is_busy.store(false, Ordering::SeqCst);
                        
                        if approved {
                            // Exit plan mode on successful approval
                            in_plan_mode = false;
                            
                            // Add synthetic assistant message so LLM knows plan was approved
                            use g3_providers::{Message, MessageRole};
                            let synthetic_msg = Message::new(MessageRole::Assistant, result);
                            agent.add_message_to_context(synthetic_msg);
                        }
                        // Stay in plan mode if approval failed
                        continue;
                    }

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
                        is_busy.store(true, Ordering::SeqCst);
                        let result = handle_command(&input, &mut agent, workspace_path, &output, &mut active_project, &mut rl, show_prompt, show_code).await?;
                        is_busy.store(false, Ordering::SeqCst);
                        
                        match result {
                            CommandResult::Handled => {
                                continue;
                            }
                            CommandResult::EnterPlanMode => {
                                in_plan_mode = true;
                                continue;
                            }
                        }
                    }

                    // Reprint input with formatting
                    reprint_formatted_input(&input, &prompt);

                    is_busy.store(true, Ordering::SeqCst);
                    execute_user_input(
                        &mut agent, &input, show_prompt, show_code, &output, from_agent_mode
                    ).await;
                    is_busy.store(false, Ordering::SeqCst);
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
        assert_eq!(prompt, " >> ");
        
        // Plan mode takes precedence over agent name
        let prompt = build_prompt(false, true, Some("butler"), &None);
        assert_eq!(prompt, " >> ");
        
        // Plan mode takes precedence over project
        let project = Some(create_test_project("myapp"));
        let prompt = build_prompt(false, true, None, &project);
        assert_eq!(prompt, " >> ");
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

    #[test]
    fn test_is_approval_input() {
        // Exact matches
        assert!(is_approval_input("a"));
        assert!(is_approval_input("approve"));
        assert!(is_approval_input("approved"));
        assert!(is_approval_input("yes"));
        assert!(is_approval_input("y"));
        assert!(is_approval_input("ok"));
        
        // Case insensitive
        assert!(is_approval_input("APPROVE"));
        assert!(is_approval_input("Approved"));
        
        // Misspellings
        assert!(is_approval_input("approv"));
        assert!(is_approval_input("aprove"));
        assert!(is_approval_input("appprove"));
        
        // Non-approval inputs
        assert!(!is_approval_input("no"));
        assert!(!is_approval_input("reject"));
        assert!(!is_approval_input("hello world"));
        
        // Should NOT match partial words in longer text
        assert!(!is_approval_input("I want to approve this"));
        assert!(!is_approval_input("please approve the plan"));
        assert!(!is_approval_input("a]ppro ve")); // gibberish with 'a' at start
        
        // Should match with trailing punctuation
        assert!(is_approval_input("approved!"));
        assert!(is_approval_input("approve."));
        assert!(is_approval_input("yes!"));
        assert!(is_approval_input("ok,"));
    }
}

