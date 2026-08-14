//! ACD (Aggressive Context Dehydration) tool: rehydrate.
//!
//! This tool allows the LLM to restore dehydrated conversation history
//! from a previous context segment.

use anyhow::Result;
use tracing::{debug, warn};

use crate::acd::Fragment;
use crate::ui_writer::UiWriter;
use crate::ToolCall;

use super::executor::ToolContext;

/// Execute the rehydrate tool.
/// Loads a fragment from disk and returns its contents for the LLM to review.
pub async fn execute_rehydrate<W: UiWriter>(
    tool_call: &ToolCall,
    ctx: &mut ToolContext<'_, W>,
) -> Result<String> {
    let fragment_id = tool_call
        .args
        .get("fragment_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required 'fragment_id' parameter"))?;

    // Get session ID from context
    let session_id = ctx
        .session_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No session ID available - cannot rehydrate fragment"))?;

    debug!("Rehydrating fragment {} for session {}", fragment_id, session_id);

    // Load the fragment
    let fragment = match Fragment::load(session_id, fragment_id) {
        Ok(f) => f,
        Err(e) => {
            warn!("Failed to load fragment {}: {}", fragment_id, e);
            return Ok(format!(
                "❌ Failed to rehydrate fragment '{}': {}\n\nThe fragment may have been deleted or the ID may be incorrect.",
                fragment_id, e
            ));
        }
    };

    // Check if rehydration would be useful (warn if context is nearly full)
    let context_percentage = (ctx.context_used_tokens as f64 / ctx.context_total_tokens as f64) * 100.0;
    let fragment_tokens = fragment.estimated_tokens;
    let available_tokens = ctx.context_total_tokens.saturating_sub(ctx.context_used_tokens);

    if fragment_tokens > available_tokens {
        return Ok(format!(
            "⚠️ Cannot rehydrate fragment '{}': it contains ~{} tokens but only {} tokens are available in context.\n\n\
            Consider compacting the context first with /compact, or continue without the full history.",
            fragment_id, fragment_tokens, available_tokens
        ));
    }

    if context_percentage > 70.0 && ctx.context_total_tokens > 0 {
        ctx.ui_writer.println(&format!(
            "⚠️ Warning: Context is at {:.0}% capacity. Rehydrating {} tokens may trigger compaction soon.",
            context_percentage, fragment_tokens
        ));
    }

    // Format the rehydrated content
    let mut output = String::new();
    output.push_str(&format!(
        "✅ Rehydrated fragment '{}' ({} messages, ~{} tokens)\n\n",
        fragment_id, fragment.message_count, fragment.estimated_tokens
    ));

    // Add fragment metadata
    output.push_str("## Fragment Metadata\n");
    output.push_str(&format!("- Created: {}\n", fragment.created_at));
    if let Some(ref preceding) = fragment.preceding_fragment_id {
        output.push_str(&format!("- Preceding fragment: {}\n", preceding));
    }
    if !fragment.topics.is_empty() {
        output.push_str(&format!("- Topics: {}\n", fragment.topics.join(", ")));
    }
    output.push_str("\n");

    // Add the conversation history
    output.push_str("## Restored Conversation\n\n");
    
    for (i, msg) in fragment.messages.iter().enumerate() {
        let role_str = match msg.role {
            g3_providers::MessageRole::User => "**User**",
            g3_providers::MessageRole::Assistant => "**Assistant**",
            g3_providers::MessageRole::System => "**System**",
        };
        
        // Truncate very long messages for readability
        let content = if msg.content.chars().count() > 2000 {
            let chars: String = msg.content.chars().take(2000).collect();
            format!("{}... [truncated, {} chars total]", chars, msg.content.chars().count())
        } else {
            msg.content.clone()
        };
        
        output.push_str(&format!("### Message {} - {}\n{}\n\n", i + 1, role_str, content));
    }

    // Add note about preceding fragments
    if fragment.preceding_fragment_id.is_some() {
        output.push_str(&format!(
            "---\n💡 This fragment has a preceding fragment. To see earlier history, call: rehydrate(fragment_id: \"{}\")\n",
            fragment.preceding_fragment_id.as_ref().unwrap()
        ));
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acd::Fragment;
    use crate::ui_writer::NullUiWriter;
    use crate::background_process::BackgroundProcessManager;
    use crate::pending_research::PendingResearchManager;
    use serial_test::serial;
    use crate::webdriver_session::WebDriverSession;
    use g3_providers::{Message, MessageRole};
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use serde_json::json;

    struct TestContext {
        ui_writer: NullUiWriter,
        webdriver_session: Arc<RwLock<Option<Arc<tokio::sync::Mutex<WebDriverSession>>>>>,
        webdriver_process: Arc<RwLock<Option<tokio::process::Child>>>,
        background_process_manager: Arc<BackgroundProcessManager>,
        todo_content: Arc<RwLock<String>>,
        pending_images: Vec<g3_providers::ImageContent>,
        pending_research_manager: PendingResearchManager,
        config: g3_config::Config,
        loaded_toolsets: std::collections::HashSet<String>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                ui_writer: NullUiWriter,
                webdriver_session: Arc::new(RwLock::new(None)),
                webdriver_process: Arc::new(RwLock::new(None)),
                background_process_manager: Arc::new(BackgroundProcessManager::new(std::path::PathBuf::from("/tmp"))),
                todo_content: Arc::new(RwLock::new(String::new())),
                pending_images: Vec::new(),
                pending_research_manager: PendingResearchManager::new(),
                config: g3_config::Config::default(),
                loaded_toolsets: std::collections::HashSet::new(),
            }
        }
    }

    #[tokio::test]
    async fn test_rehydrate_missing_fragment_id() {
        let mut test_ctx = TestContext::new();
        let mut ctx = ToolContext {
            working_dir: None,
            session_id: Some("test-session"),
            ui_writer: &test_ctx.ui_writer,
            config: &test_ctx.config,
            computer_controller: None,
            webdriver_session: &test_ctx.webdriver_session,
            webdriver_process: &test_ctx.webdriver_process,
            background_process_manager: &test_ctx.background_process_manager,
            todo_content: &test_ctx.todo_content,
            pending_research_manager: &test_ctx.pending_research_manager,
            pending_images: &mut test_ctx.pending_images,
            is_autonomous: false,
            requirements_sha: None,
            context_total_tokens: 100000,
            context_used_tokens: 10000,
            memory_path: None,
            loaded_toolsets: &mut test_ctx.loaded_toolsets,
        };

        let tool_call = ToolCall {
            tool: "rehydrate".to_string(),
            args: json!({}),
            id: String::new(),
        };

        let result = execute_rehydrate(&tool_call, &mut ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing required"));
    }

    #[tokio::test]
    async fn test_rehydrate_no_session_id() {
        let mut test_ctx = TestContext::new();
        let mut ctx = ToolContext {
            working_dir: None,
            session_id: None,
            ui_writer: &test_ctx.ui_writer,
            config: &test_ctx.config,
            computer_controller: None,
            webdriver_session: &test_ctx.webdriver_session,
            webdriver_process: &test_ctx.webdriver_process,
            background_process_manager: &test_ctx.background_process_manager,
            todo_content: &test_ctx.todo_content,
            pending_research_manager: &test_ctx.pending_research_manager,
            pending_images: &mut test_ctx.pending_images,
            is_autonomous: false,
            requirements_sha: None,
            context_total_tokens: 100000,
            context_used_tokens: 10000,
            memory_path: None,
            loaded_toolsets: &mut test_ctx.loaded_toolsets,
        };

        let tool_call = ToolCall {
            tool: "rehydrate".to_string(),
            args: json!({"fragment_id": "test-fragment"}),
            id: String::new(),
        };

        let result = execute_rehydrate(&tool_call, &mut ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No session ID"));
    }

    #[tokio::test]
    async fn test_rehydrate_nonexistent_fragment() {
        let mut test_ctx = TestContext::new();
        let mut ctx = ToolContext {
            working_dir: None,
            session_id: Some("nonexistent-session"),
            ui_writer: &test_ctx.ui_writer,
            config: &test_ctx.config,
            computer_controller: None,
            webdriver_session: &test_ctx.webdriver_session,
            webdriver_process: &test_ctx.webdriver_process,
            background_process_manager: &test_ctx.background_process_manager,
            todo_content: &test_ctx.todo_content,
            pending_research_manager: &test_ctx.pending_research_manager,
            pending_images: &mut test_ctx.pending_images,
            is_autonomous: false,
            requirements_sha: None,
            context_total_tokens: 100000,
            context_used_tokens: 10000,
            memory_path: None,
            loaded_toolsets: &mut test_ctx.loaded_toolsets,
        };

        let tool_call = ToolCall {
            tool: "rehydrate".to_string(),
            args: json!({"fragment_id": "nonexistent-fragment"}),
            id: String::new(),
        };

        let result = execute_rehydrate(&tool_call, &mut ctx).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Failed to rehydrate"));
        assert!(output.contains("nonexistent-fragment"));
    }

    #[tokio::test]
    #[serial]
    async fn test_rehydrate_success() {
        // Use a unique session ID with UUID to avoid conflicts with parallel tests
        let test_session_id = format!("test_rehydrate_{}", uuid::Uuid::new_v4());
        let messages = vec![
            Message::new(MessageRole::User, "Test user message".to_string()),
            Message::new(MessageRole::Assistant, "Test assistant response".to_string()),
        ];
        let fragment = Fragment::new(messages, None);
        let fragment_id = fragment.fragment_id.clone();
        
        // Save fragment using the Fragment::save method
        let save_result = fragment.save(&test_session_id);
        assert!(save_result.is_ok());
        let file_path = save_result.unwrap();
        assert!(file_path.exists(), "Fragment file should exist after save");
        
        // Verify we can load it back
        let loaded = Fragment::load(&test_session_id, &fragment_id);
        assert!(loaded.is_ok());
        let loaded_fragment = loaded.unwrap();
        assert_eq!(loaded_fragment.fragment_id, fragment_id);
        assert_eq!(loaded_fragment.message_count, 2);
        
        // Cleanup
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
            // Also try to remove the session directory
            let _ = std::fs::remove_dir_all(parent.parent().unwrap_or(parent));
        }
    }
}
