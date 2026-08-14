//! Tool executor trait and context for tool execution.

use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::pending_research::PendingResearchManager;
use crate::background_process::BackgroundProcessManager;
use crate::paths::{ensure_session_dir, get_session_todo_path, get_todo_path};
use crate::ui_writer::UiWriter;
use crate::webdriver_session::WebDriverSession;
use crate::ToolCall;
use g3_config::Config;

/// Context passed to tool executors containing shared state.
pub struct ToolContext<'a, W: UiWriter> {
    pub config: &'a Config,
    pub ui_writer: &'a W,
    pub session_id: Option<&'a str>,
    pub working_dir: Option<&'a str>,
    pub computer_controller: Option<&'a Box<dyn g3_computer_control::ComputerController>>,
    pub webdriver_session: &'a Arc<RwLock<Option<Arc<tokio::sync::Mutex<WebDriverSession>>>>>,
    pub webdriver_process: &'a Arc<RwLock<Option<tokio::process::Child>>>,
    pub background_process_manager: &'a Arc<BackgroundProcessManager>,
    pub todo_content: &'a Arc<RwLock<String>>,
    pub pending_research_manager: &'a PendingResearchManager,
    pub pending_images: &'a mut Vec<g3_providers::ImageContent>,
    pub is_autonomous: bool,
    pub requirements_sha: Option<&'a str>,
    pub context_total_tokens: u32,
    pub context_used_tokens: u32,
    /// Override for the workspace memory file location (`--memory <path>`).
    /// `None` means the default `analysis/memory.md`. Must be passed to
    /// `tools::memory::resolve_memory_path` — never joined by hand.
    pub memory_path: Option<&'a str>,
    /// Set of toolset names that have been loaded in this session
    pub loaded_toolsets: &'a mut HashSet<String>,
}

impl<'a, W: UiWriter> ToolContext<'a, W> {
    /// Get the path to the TODO file (session-scoped or workspace).
    pub fn get_todo_path(&self) -> std::path::PathBuf {
        if let Some(session_id) = self.session_id {
            let _ = ensure_session_dir(session_id);
            get_session_todo_path(session_id)
        } else {
            get_todo_path()
        }
    }
}

/// Trait for tool executors.
/// Each tool category implements this trait.
pub trait ToolExecutor<W: UiWriter> {
    /// Execute a tool call and return the result.
    /// Returns None if this executor doesn't handle the given tool.
    fn execute<'a>(
        tool_call: &'a ToolCall,
        ctx: &'a mut ToolContext<'_, W>,
    ) -> impl std::future::Future<Output = Option<Result<String>>> + Send + 'a
    where
        W: 'a;
}
