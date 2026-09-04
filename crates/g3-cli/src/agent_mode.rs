//! Agent mode for G3 CLI - runs specialized agents with custom prompts.

use anyhow::Result;
use tracing::debug;

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;

use crate::project_files::{combine_project_content, discover_and_format_skills, read_agents_config, read_include_prompt, read_workspace_memory};
use crate::display::{LoadedContent, print_loaded_status, print_workspace_path};
use crate::language_prompts::{get_language_prompts_for_workspace, get_agent_language_prompts_for_workspace_with_langs};
use crate::simple_output::SimpleOutput;
use crate::embedded_agents::load_agent_prompt;
use crate::ui_writer_impl::ConsoleUiWriter;
use crate::interactive::run_interactive;
use crate::template::process_template;
use crate::project::{Project, load_and_validate_project};
use crate::cli_args::CommonFlags;

/// Run agent mode - loads a specialized agent prompt and executes a single task.
/// 
/// Uses `CommonFlags` for flags that apply across all modes, ensuring consistency.
pub async fn run_agent_mode(
    agent_name: &str,
    task: Option<String>,
    chat: bool,
    flags: CommonFlags,
) -> Result<()> {
    use g3_core::find_incomplete_agent_session;
    use g3_core::get_agent_system_prompt;

    // Set process title to agent name (shows in ps, Activity Monitor, etc.)
    proctitle::set_title(format!("g3 [{}]", agent_name));

    let output = SimpleOutput::new();

    // Determine workspace directory (current dir if not specified)
    let workspace_dir = flags.workspace.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // Change to the workspace directory first so session scanning works correctly
    std::env::set_current_dir(&workspace_dir)?;

    // Check for incomplete agent sessions before starting a new one
    // When --resume is explicitly provided, always honor it (even in chat mode)
    // Otherwise, chat mode starts fresh (no auto-resume of incomplete sessions)
    let resuming_session = if let Some(ref session_id) = flags.resume {
        // Explicit --resume flag takes precedence
        match g3_core::load_continuation_by_id(session_id) {
            Ok(continuation) => {
                // Verify the session matches this agent (or allow any if agent name matches)
                if continuation.agent_name.as_deref() != Some(agent_name) {
                    eprintln!("Error: Session '{}' belongs to agent '{}', not '{}'",
                        session_id,
                        continuation.agent_name.as_deref().unwrap_or("(none)"),
                        agent_name);
                    std::process::exit(1);
                }
                Some(continuation)
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    } else if chat {
        // Chat mode without explicit --resume starts fresh (no auto-resume)
        None
    } else if flags.new_session {
        if !chat {
            output.print("\n🆕 Starting new session (--new-session flag set)");
            output.print("");
        }
        None
    } else {
        find_incomplete_agent_session(agent_name).ok().flatten()
    };

    // Only show session resume info when not in chat mode
    if !chat {
      if let Some(ref incomplete_session) = resuming_session {
        output.print(&format!(
            "\n🔄 Found incomplete session for agent '{}'",
            agent_name
        ));
        output.print(&format!("   Session: {}", incomplete_session.session_id));
        output.print(&format!("   Created: {}", incomplete_session.created_at));
        if let Some(ref todo) = incomplete_session.todo_snapshot {
            // Show first few lines of TODO
            let preview: String = todo.lines().take(5).collect::<Vec<_>>().join("\n");
            output.print(&format!("   TODO preview:\n{}", preview));
        }
        output.print("");
        output.print("   Resuming incomplete session...");
        output.print("");
      }
    }

    // Load agent prompt: workspace agents/<name>.md first, then embedded fallback
    let (agent_prompt, from_disk) = load_agent_prompt(agent_name, &workspace_dir).ok_or_else(|| {
        anyhow::anyhow!(
            "Agent '{}' not found.\nAvailable embedded agents: breaker, carmack, euler, fowler, hopper, lamport, scout, solon\nOr create agents/{}.md in your workspace.",
            agent_name,
            agent_name
        )
    })?;

    let source = if from_disk { "workspace" } else { "embedded" };
    // Only print verbose header when not in chat mode
    if !chat {
        output.print(&format!(">> agent mode | {} ({})", agent_name, source));
    }
    // Always print workspace path (it's part of minimal output)
    print_workspace_path(&workspace_dir);

    // Load config
    let mut config = g3_config::Config::load(flags.config.as_deref())?;

    // Apply thinning-floor override (e.g. scout's aggressive 5%).
    if let Some(floor) = flags.thinning_floor {
        config.agent.thinning_floor_percent = floor;
    }

    // Apply chrome-headless flag override
    if flags.chrome_headless {
        config.webdriver.enabled = true;
        config.webdriver.browser = g3_config::WebDriverBrowser::ChromeHeadless;
    }

    // Apply safari flag override
    if flags.safari {
        config.webdriver.enabled = true;
        config.webdriver.browser = g3_config::WebDriverBrowser::Safari;
    }

    // Generate the combined system prompt (agent prompt + tool instructions)
    // Note: allow_multiple_tool_calls parameter is deprecated but kept for API compatibility
    let system_prompt = get_agent_system_prompt(&agent_prompt, true);

    // Load AGENTS.md and memory - same as normal mode
    let agents_content_opt = read_agents_config(&workspace_dir);
    let memory_content_opt = read_workspace_memory(
        &workspace_dir,
        flags.memory_path.as_deref().and_then(|p| p.to_str()),
    );

    // Read include prompt early so we can show it in the status line
    let include_prompt = read_include_prompt(flags.include_prompt.as_deref());

    // Build and print status line showing what was loaded
    let include_filename = flags.include_prompt.as_ref()
        .filter(|_| include_prompt.is_some())
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string());
    let loaded = LoadedContent::new(
        agents_content_opt.is_some(),
        memory_content_opt.is_some(),
        include_filename,
    );
    print_loaded_status(&loaded);

    // Get language-specific prompts (same mechanism as normal mode)
    let language_content = get_language_prompts_for_workspace(&workspace_dir);

    // Get agent+language-specific prompts (e.g., carmack.racket.md) and show which languages
    let detected_langs = crate::language_prompts::detect_languages(&workspace_dir);
    let agent_lang_content = if detected_langs.is_empty() {
        None
    } else {
        let (content, matched_langs) = get_agent_language_prompts_for_workspace_with_langs(&workspace_dir, agent_name);
        // Only print language guidance info when not in chat mode
        if !chat {
            for lang in matched_langs {
                output.print(&format!("   ✓ {}: {} language guidance", agent_name, lang));
            }
        }
        content
    };

    // Append agent+language-specific content to system prompt if available
    let system_prompt = if let Some(agent_lang) = agent_lang_content {
        format!("{}\n\n{}", system_prompt, agent_lang)
    } else {
        system_prompt
    };

    // Discover skills from configured paths
    let (_skills, skills_content) = discover_and_format_skills(&workspace_dir, &config.skills);

    // Combine all content for the agent's context
    let combined_content = combine_project_content(
        agents_content_opt,
        memory_content_opt,
        language_content,
        include_prompt,
        skills_content,
        &workspace_dir,
    );

    // Create agent with custom system prompt
    let ui_writer = ConsoleUiWriter::new();
    // Set agent mode on UI writer for visual differentiation (light gray tool names)
    ui_writer.set_agent_mode(true);
    ui_writer.set_workspace_path(workspace_dir.clone());
    // Wrap in EventStreamWriter — if --stream-events was passed, events will be
    // teed to that file; otherwise this is a zero-cost pass-through.
    let ui_writer = crate::event_stream::EventStreamWriter::new(
        ui_writer,
        flags.stream_events.as_deref(),
    );
    let mut agent =
        Agent::new_with_custom_prompt(config, ui_writer, system_prompt, combined_content.clone()).await?;

    // Set agent mode for session tracking
    agent.set_agent_mode(agent_name);

    // Keep the write path (remember) pointed at the same file read_workspace_memory
    // loaded from above. If this is ever dropped, memory forks silently.
    agent.set_memory_path(
        flags.memory_path.as_ref().map(|p| p.to_string_lossy().to_string()),
    );

    // Auto-memory is enabled by default in agent mode (unless --no-auto-memory is set)
    // This prompts the LLM to save discoveries to workspace memory after each turn
    agent.set_auto_memory(!flags.no_auto_memory);
    
    // Enable ACD (Aggressive Context Dehydration) if requested
    if flags.acd {
        agent.set_acd_enabled(true);
    }

    // If resuming a session, restore context and TODO
    let initial_task = if let Some(ref incomplete_session) = resuming_session {
        // Restore the session context
        match agent.restore_from_continuation(incomplete_session) {
            Ok(full_restore) => {
                if full_restore {
                    output.print("   ✅ Full context restored from previous session");
                } else {
                    output.print("   ⚠️ Restored from summary (context was > 80%)");
                }
            }
            Err(e) => {
                output.print(&format!("   ⚠️ Could not restore context: {}", e));
            }
        }

        // Copy TODO from old session to new session directory
        let todo_content = if let Some(ref content) = incomplete_session.todo_snapshot {
            Some(content.clone())
        } else {
            // Fallback: read from the actual todo.g3.md file in the old session directory
            let old_session_dir =
                std::path::Path::new(".g3/sessions").join(&incomplete_session.session_id);
            let old_todo_path = old_session_dir.join("todo.g3.md");
            if old_todo_path.exists() {
                std::fs::read_to_string(&old_todo_path).ok()
            } else {
                None
            }
        };

        if let Some(ref content) = todo_content {
            if let Some(session_id) = agent.get_session_id() {
                let new_todo_path = g3_core::paths::get_session_todo_path(session_id);
                let _ = g3_core::paths::ensure_session_dir(session_id);
                if let Err(e) = std::fs::write(&new_todo_path, content) {
                    output.print(&format!("   ⚠️ Could not restore TODO: {}", e));
                } else {
                    output.print("   ✅ TODO list restored");
                }
            }
        }
        output.print("");

        // Resume message instead of fresh start
        "Continue working on the incomplete tasks. Use todo_read to see the current TODO list and resume from where you left off."
    } else {
        // Fresh start - the agent prompt should contain instructions to start working immediately
        "Begin your analysis and work on the current project. Follow your mission and workflow as specified in your instructions."
    };
    // Use provided task if available, otherwise use the default initial_task
    let task_str = task.as_deref().unwrap_or(initial_task);
    let final_task = process_template(task_str);

    // If chat mode is enabled, run interactive loop instead of single task
    if chat {
        // Load project if --project flag was specified
        let initial_project: Option<Project> = if let Some(ref proj_path) = flags.project {
            match load_and_validate_project(&proj_path.to_string_lossy(), &workspace_dir) {
                Ok(cli_project) => {
                    // Set project content in agent's system message
                    if agent.set_project_content(Some(cli_project.content.clone())) {
                        // Set project path on UI writer for path shortening
                        let project_name = cli_project.path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("project")
                            .to_string();
                        agent.ui_writer().set_project_path(cli_project.path.clone(), project_name);
                        Some(cli_project)
                    } else {
                        eprintln!("Warning: Failed to set project content in agent context.");
                        None
                    }
                }
                Err(e) => {
                    eprintln!("Error loading project: {}", e);
                    std::process::exit(1);
                }
            }
        } else {
            None
        };

        return run_interactive(
            agent,
            false, // show_prompt
            false, // show_code
            combined_content,
            &workspace_dir,
            Some(agent_name),  // agent name for prompt (e.g., "butler>")
            initial_project,
        )
        .await;
    }

    // Single-shot mode: execute the task and exit
    let _result = agent.execute_task(&final_task, None, true).await?;

    // Send auto-memory reminder if enabled and tools were called
    if let Err(e) = agent.send_auto_memory_reminder().await {
        debug!("Auto-memory reminder failed: {}", e);
    }

    // Save session continuation for resume capability
    agent.save_session_continuation(None);

    // Emit end-of-turn context window summary (renders locally + tees NDJSON
    // event when --stream-events is active, e.g. from butler.app).
    {
        let cw = agent.get_context_window();
        agent
            .ui_writer()
            .print_context_summary(cw.used_tokens, cw.total_tokens, cw.percentage_used());
    }

    // Don't print completion message for scout agent - it needs the last line
    // to be the report file path for the research tool to read
    if agent_name != "scout" {
        use crate::g3_status::G3Status;
        println!(); // newline before status
        G3Status::progress(&format!("{} session", agent_name));
        G3Status::done();
    }
    Ok(())
}
