pub mod acd;
pub mod background_process;
pub mod code_search;
pub mod compaction;
pub mod context_window;
pub mod error_handling;
pub mod feedback_extraction;
pub mod paths;
pub mod project;
pub mod pending_input;
pub mod pending_research;
pub mod provider_config;
pub mod provider_registration;
pub mod retry;
pub mod session;
pub mod session_continuation;
pub mod stats;
pub mod streaming;
pub mod streaming_parser;
pub mod task_result;
pub mod tool_definitions;
pub mod tool_dispatch;
pub mod tools;
pub mod ui_writer;
pub mod utils;
pub mod webdriver_session;
pub mod skills;
pub mod toolsets;

pub use feedback_extraction::{
    extract_coach_feedback, ExtractedFeedback, FeedbackExtractionConfig, FeedbackSource,
};
pub use retry::{execute_with_retry, retry_operation, RetryConfig, RetryResult};
pub use session_continuation::{
    clear_continuation, find_incomplete_agent_session, format_session_time, get_session_dir,
    has_valid_continuation, list_sessions_for_directory, load_context_from_session_log,
    load_continuation, load_continuation_by_id, save_continuation, SessionContinuation,
};
pub use task_result::TaskResult;

// Re-export context window types
pub use context_window::{ContextWindow, ThinResult, ThinScope};

// Export agent prompt generation for CLI use
pub use prompts::{
    get_agent_system_prompt, get_agent_system_prompt_with_skills,
    get_system_prompt_for_native_with_skills, get_system_prompt_for_non_native_with_skills,
};

// Export skills module
pub use skills::{Skill, discover_skills, generate_skills_prompt};

#[cfg(test)]
mod task_result_comprehensive_tests;
use crate::ui_writer::UiWriter;
use tools::plan::{check_plan_approval_gate, get_dirty_files, read_plan, ApprovalGateResult};

#[cfg(test)]
mod tilde_expansion_tests;

#[cfg(test)]
mod error_handling_test;
mod prompts;

use anyhow::Result;
use g3_config::Config;
use g3_providers::{CacheControl, CompletionRequest, Message, MessageRole, ProviderRegistry};
use prompts::{get_system_prompt_for_native, get_system_prompt_for_non_native};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

// Re-export path utilities
use paths::get_todo_path;
pub use paths::{
    ensure_session_dir, get_background_processes_dir, get_context_summary_file, get_discovery_dir,
    get_errors_dir, get_g3_dir, get_session_file, get_session_logs_dir, get_session_todo_path,
    get_thinned_dir, G3_WORKSPACE_PATH_ENV,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value, // Should be a JSON object with tool-specific arguments
    /// Unique ID for this tool call (from native tool calling providers).
    /// Used to correlate tool_use/tool_result blocks in the API.
    #[serde(default)]
    pub id: String,
}

/// Cumulative cache statistics for prompt caching efficacy tracking.
/// Tracks both Anthropic-style (cache_creation + cache_read) and OpenAI-style (cached_tokens) caching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    /// Total tokens written to cache across all API calls
    pub total_cache_creation_tokens: u64,
    /// Total tokens read from cache across all API calls
    pub total_cache_read_tokens: u64,
    /// Total input tokens (for calculating cache hit rate)
    pub total_input_tokens: u64,
    /// Number of API calls that had cache hits
    pub cache_hit_calls: u32,
    /// Total number of API calls
    pub total_calls: u32,
}

// Re-export WebDriverSession from its own module
pub use webdriver_session::WebDriverSession;

/// Options for fast-start discovery execution
#[derive(Debug, Clone)]
pub struct DiscoveryOptions<'a> {
    pub messages: &'a [Message],
    pub fast_start_path: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub enum StreamState {
    Generating,
    ToolDetected(ToolCall),
    Executing,
    Resuming,
}

// Re-export StreamingToolParser from its own module
pub use streaming_parser::StreamingToolParser;

pub struct Agent<W: UiWriter> {
    providers: ProviderRegistry,
    context_window: ContextWindow,
    thinning_events: Vec<usize>,      // chars saved per thinning event
    pending_90_compaction: bool,      // flag to trigger compaction at 90%
    auto_compact: bool,               // whether to auto-compact at 90% before tool calls
    compaction_events: Vec<usize>,    // chars saved per compaction event
    first_token_times: Vec<Duration>, // time to first token for each completion
    /// Cumulative cache statistics across all API calls
    cache_stats: CacheStats,
    config: Config,
    session_id: Option<String>,
    tool_call_metrics: Vec<(String, Duration, bool)>, // (tool_name, duration, success)
    ui_writer: W,
    is_autonomous: bool,
    quiet: bool,
    computer_controller: Option<Box<dyn g3_computer_control::ComputerController>>,
    todo_content: std::sync::Arc<tokio::sync::RwLock<String>>,
    webdriver_session: std::sync::Arc<
        tokio::sync::RwLock<Option<std::sync::Arc<tokio::sync::Mutex<WebDriverSession>>>>,
    >,
    webdriver_process: std::sync::Arc<tokio::sync::RwLock<Option<tokio::process::Child>>>,
    tool_call_count: usize,
    /// Tool calls made in the current turn (reset after each turn)
    tool_calls_this_turn: Vec<String>,
    requirements_sha: Option<String>,
    /// Working directory for tool execution (set by --codebase-fast-start)
    working_dir: Option<String>,
    background_process_manager: std::sync::Arc<background_process::BackgroundProcessManager>,
    /// Pending images to attach to the next user message
    pending_images: Vec<g3_providers::ImageContent>,
    /// Whether this agent is running in agent mode (--agent flag)
    is_agent_mode: bool,
    /// Name of the agent if running in agent mode (e.g., "fowler", "pike")
    agent_name: Option<String>,
    /// Whether auto-memory reminders are enabled (--auto-memory flag)
    auto_memory: bool,
    /// Whether aggressive context dehydration is enabled (--acd flag)
    acd_enabled: bool,
    /// Whether plan mode is active (gate blocks file changes without approved plan)
    in_plan_mode: bool,
    /// Files that were already dirty when plan mode started (excluded from approval gate)
    baseline_dirty_files: std::collections::HashSet<String>,
    /// Manager for async research tasks
    pending_research_manager: pending_research::PendingResearchManager,
    /// Set of toolset names that have been loaded in this session
    loaded_toolsets: std::collections::HashSet<String>,
}

impl<W: UiWriter> Agent<W> {
    // =========================================================================
    // CONSTRUCTION METHODS
    // =========================================================================

    /// Internal helper to construct an Agent with all fields.
    /// This is the single canonical constructor - all public constructors delegate here.
    /// This eliminates code duplication and ensures all Agent instances are built consistently.
    fn build_agent(
        config: Config,
        ui_writer: W,
        providers: ProviderRegistry,
        context_window: ContextWindow,
        auto_compact: bool,
        is_autonomous: bool,
        quiet: bool,
        computer_controller: Option<Box<dyn g3_computer_control::ComputerController>>,
    ) -> Self {
        Self {
            providers,
            context_window,
            auto_compact,
            pending_90_compaction: false,
            thinning_events: Vec::new(),
            compaction_events: Vec::new(),
            first_token_times: Vec::new(),
            cache_stats: CacheStats::default(),
            config,
            session_id: None,
            tool_call_metrics: Vec::new(),
            ui_writer,
            todo_content: std::sync::Arc::new(tokio::sync::RwLock::new(String::new())),
            is_autonomous,
            quiet,
            computer_controller,
            webdriver_session: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            webdriver_process: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            tool_call_count: 0,
            tool_calls_this_turn: Vec::new(),
            requirements_sha: None,
            working_dir: None,
            background_process_manager: std::sync::Arc::new(
                background_process::BackgroundProcessManager::new(
                    paths::get_background_processes_dir(),
                ),
            ),
            pending_images: Vec::new(),
            is_agent_mode: false,
            agent_name: None,
            auto_memory: false,
            acd_enabled: false,
            in_plan_mode: false,
            baseline_dirty_files: std::collections::HashSet::new(),
            pending_research_manager: pending_research::PendingResearchManager::new(),
            loaded_toolsets: std::collections::HashSet::new(),
        }
    }

    pub async fn new(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, false, false).await
    }

    pub async fn new_autonomous(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, true, false).await
    }

    pub async fn new_with_project_context_and_quiet(
        config: Config,
        ui_writer: W,
        project_context: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_project_context(config, ui_writer, false, project_context, quiet, None).await
    }

    pub async fn new_autonomous_with_project_context_and_quiet(
        config: Config,
        ui_writer: W,
        project_context: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_project_context(config, ui_writer, true, project_context, quiet, None).await
    }

    /// Create a new agent with a custom system prompt (for agent mode)
    /// The custom_system_prompt replaces the default G3 system prompt entirely
    pub async fn new_with_custom_prompt(
        config: Config,
        ui_writer: W,
        custom_system_prompt: String,
        project_context: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_project_context(
            config,
            ui_writer,
            false,
            project_context,
            false,
            Some(custom_system_prompt),
        )
        .await
    }

    /// Create a new agent with a custom provider registry (for testing).
    /// This allows tests to inject mock providers without needing real API credentials.
    /// 
    /// **Note**: This method is intended for testing only. Do not use in production code.
    #[doc(hidden)]
    pub async fn new_for_test(
        config: Config,
        ui_writer: W,
        providers: ProviderRegistry,
    ) -> Result<Self> {
        Self::new_for_test_with_project_context(config, ui_writer, providers, None).await
    }

    /// Create a new agent for testing with project context.
    /// This allows tests to verify context window structure with combined content.
    #[doc(hidden)]
    pub async fn new_for_test_with_project_context(
        config: Config,
        ui_writer: W,
        providers: ProviderRegistry,
        project_context: Option<String>,
    ) -> Result<Self> {
        use crate::context_window::ContextWindow;
        use crate::prompts::get_system_prompt_for_native;
        use g3_providers::{Message, MessageRole};

        let context_length = config.agent.max_context_length.unwrap_or(200_000);
        let mut context_window = ContextWindow::with_threshold(
            context_length,
            config.agent.compaction_threshold_percent,
        );

        // Add system prompt
        let system_prompt = get_system_prompt_for_native();
        let system_message = Message::new(MessageRole::System, system_prompt);
        context_window.add_message(system_message);

        // Add project context if provided
        if let Some(context) = project_context {
            let context_message = Message::new(MessageRole::System, context);
            context_window.add_message(context_message);
        }

        // For tests: auto_compact=false, is_autonomous=false, quiet=true, no computer_controller
        Ok(Self::build_agent(
            config,
            ui_writer,
            providers,
            context_window,
            false, // auto_compact
            false, // is_autonomous
            true,  // quiet
            None,  // computer_controller
        ))
    }

    async fn new_with_mode(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_project_context(config, ui_writer, is_autonomous, None, quiet, None).await
    }

    async fn new_with_mode_and_project_context(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        project_context: Option<String>,
        quiet: bool,
        custom_system_prompt: Option<String>,
    ) -> Result<Self> {
        // Register providers using the extracted module
        let providers_to_register =
            provider_registration::determine_providers_to_register(&config, is_autonomous);
        let providers =
            provider_registration::register_providers(&config, &providers_to_register).await?;

        // Determine context window size based on active provider
        let mut context_warnings = Vec::new();
        let context_length =
            Self::get_configured_context_length(&config, &providers, &mut context_warnings)?;
        let mut context_window = ContextWindow::with_threshold(
            context_length,
            config.agent.compaction_threshold_percent,
        );

        // Surface any context warnings to the user via UI
        for warning in context_warnings {
            ui_writer.print_context_status(&format!("⚠️ {}", warning));
        }

        // Add system prompt as the FIRST message (before README)
        // This ensures the agent always has proper tool usage instructions
        let provider = providers.get(None)?;
        let provider_has_native_tool_calling = provider.has_native_tool_calling();
        let _ = provider; // Drop provider reference to avoid borrowing issues

        let system_prompt = if let Some(custom_prompt) = custom_system_prompt {
            // Use custom system prompt (for agent mode)
            custom_prompt
        } else {
            // Use default system prompt based on provider capabilities
            if provider_has_native_tool_calling {
                // For native tool calling providers, use a more explicit system prompt
                get_system_prompt_for_native()
            } else {
                // For non-native providers (embedded models), use JSON format instructions
                get_system_prompt_for_non_native()
            }
        };

        let system_message = Message::new(MessageRole::System, system_prompt);
        context_window.add_message(system_message);

        // If project context is provided, add it as a second system message (after the main system prompt)
        if let Some(context) = project_context {
            let context_message = Message::new(MessageRole::System, context);
            context_window.add_message(context_message);
        }

        // NOTE: TODO lists are now session-scoped and stored in .g3/sessions/<session_id>/todo.g3.md
        // We don't load any TODO at initialization since we don't have a session_id yet.
        // The agent will use todo_read to load the TODO once a session is established.

        // Initialize computer controller if enabled
        let computer_controller = if config.computer_control.enabled {
            match g3_computer_control::create_controller() {
                Ok(controller) => Some(controller),
                Err(e) => {
                    warn!("Failed to initialize computer control: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let auto_compact = config.agent.auto_compact;
        Ok(Self::build_agent(
            config,
            ui_writer,
            providers,
            context_window,
            auto_compact,
            is_autonomous,
            quiet,
            computer_controller,
        ))
    }

    // =========================================================================
    // CONFIGURATION & PROVIDER RESOLUTION
    // =========================================================================

    /// Validate that the system prompt is the first message in the conversation history.
    /// This is a critical invariant that must be maintained for proper agent operation.
    ///
    /// # Panics
    /// Panics if:
    /// - The conversation history is empty
    /// - The first message is not a System message
    /// - The first message doesn't contain the system prompt markers
    fn validate_system_prompt_is_first(&self) {
        if self.context_window.conversation_history.is_empty() {
            panic!(
                "FATAL: Conversation history is empty. System prompt must be the first message."
            );
        }

        let first_message = &self.context_window.conversation_history[0];

        if !matches!(first_message.role, MessageRole::System) {
            panic!(
                "FATAL: First message is not a System message. Found: {:?}",
                first_message.role
            );
        }

        // Check for system prompt markers that are present in both standard and agent mode
        // Check for system prompt markers that are present in both native and non-native prompts
        // Both prompts contain "Use tools to accomplish tasks" as a common marker
        let has_tool_instructions = first_message
            .content
            .contains("Use tools to accomplish tasks");
        if !has_tool_instructions {
            panic!("FATAL: First system message does not contain the system prompt marker 'Use tools to accomplish tasks'. This likely means the README was added before the system prompt.");
        }
    }

    /// Convert cache config string to CacheControl enum
    fn parse_cache_control(cache_config: &str) -> Option<CacheControl> {
        match cache_config {
            "ephemeral" => Some(CacheControl::ephemeral()),
            "5minute" => Some(CacheControl::five_minute()),
            "1hour" => Some(CacheControl::one_hour()),
            _ => {
                warn!(
                    "Invalid cache_config value: '{}'. Valid values are: ephemeral, 5minute, 1hour",
                    cache_config
                );
                None
            }
        }
    }

    /// Count how many cache_control annotations exist in the conversation history
    fn count_cache_controls_in_history(&self) -> usize {
        self.context_window
            .conversation_history
            .iter()
            .filter(|msg| msg.cache_control.is_some())
            .count()
    }

    /// Get the cache control config for the current provider (if Anthropic with cache enabled).
    fn get_provider_cache_control(&self) -> Option<CacheControl> {
        let provider = self.providers.get(None).ok()?;
        let provider_name = provider.name();
        let (provider_type, config_name) = provider_config::parse_provider_ref(provider_name);

        match provider_type {
            "anthropic" => self
                .config
                .providers
                .anthropic
                .get(config_name)
                .and_then(|c| c.cache_config.as_ref())
                .and_then(|config| Self::parse_cache_control(config)),
            _ => None,
        }
    }

    /// Resolve the max_tokens to use for a given provider, applying fallbacks.
    fn resolve_max_tokens(&self, provider_name: &str) -> u32 {
        provider_config::resolve_max_tokens(&self.config, provider_name)
    }

    /// Get the thinking budget tokens for Anthropic provider, if configured.
    /// Pre-flight check to validate max_tokens for thinking.budget_tokens constraint.
    fn preflight_validate_max_tokens(
        &self,
        provider_name: &str,
        proposed_max_tokens: u32,
    ) -> (u32, bool) {
        provider_config::preflight_validate_max_tokens(
            &self.config,
            provider_name,
            proposed_max_tokens,
        )
    }

    /// Calculate max_tokens for a summary request.
    fn calculate_summary_max_tokens(&self, provider_name: &str) -> (u32, bool) {
        provider_config::calculate_summary_max_tokens(
            &self.config,
            provider_name,
            self.context_window.total_tokens,
            self.context_window.used_tokens,
        )
    }

    /// Apply the fallback sequence to free up context space for thinking budget.
    fn apply_max_tokens_fallback_sequence(
        &mut self,
        provider_name: &str,
        initial_max_tokens: u32,
        hard_coded_minimum: u32,
    ) -> u32 {
        self.apply_fallback_sequence_impl(
            provider_name,
            Some(initial_max_tokens),
            hard_coded_minimum,
        )
    }

    /// Unified implementation of the fallback sequence for freeing context space.
    /// If `initial_max_tokens` is Some, uses preflight_validate_max_tokens for validation.
    /// If `initial_max_tokens` is None, uses calculate_summary_max_tokens for validation.
    fn apply_fallback_sequence_impl(
        &mut self,
        provider_name: &str,
        initial_max_tokens: Option<u32>,
        hard_coded_minimum: u32,
    ) -> u32 {
        // Initial validation
        let (mut max_tokens, needs_reduction) = match initial_max_tokens {
            Some(initial) => self.preflight_validate_max_tokens(provider_name, initial),
            None => self.calculate_summary_max_tokens(provider_name),
        };

        if !needs_reduction {
            return max_tokens;
        }

        self.ui_writer.print_context_status(
            "⚠️ Context window too full for thinking budget. Applying fallback sequence...\n",
        );

        // Step 1: Try thinnify (first third of context)
        self.ui_writer
            .print_context_status("🥒 Step 1: Trying thinnify...\n");
        let thin_result = self.do_thin_context();
        self.ui_writer.print_thin_result(&thin_result);

        // Recalculate after thinnify
        let (new_max, still_needs_reduction) =
            self.recalculate_max_tokens(provider_name, initial_max_tokens.is_some());
        max_tokens = new_max;
        if !still_needs_reduction {
            self.ui_writer
                .print_context_status("✅ Thinnify resolved capacity issue. Continuing...\n");
            return max_tokens;
        }

        // Step 2: Try skinnify (entire context)
        self.ui_writer
            .print_context_status("🦴 Step 2: Trying skinnify...\n");
        let skinny_result = self.do_thin_context_all();
        self.ui_writer.print_thin_result(&skinny_result);

        // Recalculate after skinnify
        let (final_max, final_needs_reduction) =
            self.recalculate_max_tokens(provider_name, initial_max_tokens.is_some());
        if !final_needs_reduction {
            self.ui_writer
                .print_context_status("✅ Skinnify resolved capacity issue. Continuing...\n");
            return final_max;
        }

        // Step 3: Nothing worked, use hard-coded minimum
        self.ui_writer.print_context_status(&format!(
            "⚠️ Step 3: Context reduction insufficient. Using hard-coded max_tokens={} as last resort...\n",
            hard_coded_minimum
        ));
        hard_coded_minimum
    }

    /// Helper to recalculate max_tokens after context reduction.
    fn recalculate_max_tokens(&self, provider_name: &str, use_preflight: bool) -> (u32, bool) {
        if use_preflight {
            let recalc_max = self.resolve_max_tokens(provider_name);
            self.preflight_validate_max_tokens(provider_name, recalc_max)
        } else {
            self.calculate_summary_max_tokens(provider_name)
        }
    }

    /// Resolve the temperature to use for a given provider, applying fallbacks.
    fn resolve_temperature(&self, provider_name: &str) -> f32 {
        provider_config::resolve_temperature(&self.config, provider_name)
    }

    /// Get tool definitions including any dynamically loaded toolsets.
    fn get_tool_definitions_with_loaded_toolsets(&self, tool_config: tool_definitions::ToolConfig) -> Vec<g3_providers::Tool> {
        let mut tools = tool_definitions::create_tool_definitions(tool_config);
        
        // Add tools from loaded toolsets
        for toolset_name in &self.loaded_toolsets {
            if let Ok(toolset) = toolsets::get_toolset(toolset_name) {
                let toolset_tools = toolset.get_tools();
                // Avoid duplicates (in case toolset was already in base config)
                for tool in toolset_tools {
                    if !tools.iter().any(|t| t.name == tool.name) {
                        tools.push(tool);
                    }
                }
            }
        }
        
        tools
    }

    /// Print provider diagnostics through the UiWriter for visibility
    pub fn print_provider_banner(&self, role_label: &str) {
        if let Ok((provider_name, model)) = self.get_provider_info() {
            let max_tokens = self.resolve_max_tokens(&provider_name);
            let context_len = self.context_window.total_tokens;

            let mut details = vec![
                format!("provider={}", provider_name),
                format!("model={}", model),
                format!("max_tokens={}", max_tokens),
                format!("context_window_length={}", context_len),
            ];

            if let Ok(provider) = self.providers.get(None) {
                details.push(format!(
                    "native_tools={}",
                    if provider.has_native_tool_calling() {
                        "yes"
                    } else {
                        "no"
                    }
                ));
                if provider.supports_cache_control() {
                    details.push("cache_control=yes".to_string());
                }
            }

            self.ui_writer
                .print_context_status(&format!("{}: {}", role_label, details.join(", ")));
        }
    }

    fn get_configured_context_length(
        config: &Config,
        providers: &ProviderRegistry,
        warnings: &mut Vec<String>,
    ) -> Result<u32> {
        // First, check if there's a global max_context_length override in agent config
        if let Some(max_context_length) = config.agent.max_context_length {
            debug!(
                "Using configured agent.max_context_length: {}",
                max_context_length
            );
            return Ok(max_context_length);
        }

        // Get the active provider to determine context length
        let provider = providers.get(None)?;
        let provider_name = provider.name();
        let model_name = provider.model();

        // Parse provider name to get type and config name
        let (provider_type, _config_name) = provider_config::parse_provider_ref(provider_name);

        // Use provider-specific context length if available
        let context_length = match provider_type {
            "embedded" | "embedded." => {
                // For embedded models, query the provider directly for its context window
                // The provider auto-detects this from the GGUF file
                if let Some(ctx_size) = provider.context_window_size() {
                    debug!(
                        "Using context window size {} from embedded provider",
                        ctx_size
                    );
                    ctx_size
                } else {
                    config.agent.fallback_default_max_tokens as u32
                }
            }
            "openai" => {
                // OpenAI models have varying context windows
                if let Some(max_tokens) = provider_config::get_max_tokens(config, provider_name) {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else {
                    400000
                }
            }
            "anthropic" => {
                // Claude models have large context windows.
                //
                // Precedence note: `enable_1m_context` is checked BEFORE the
                // max_tokens fallback. max_tokens is an *output* limit that this
                // function only borrows as a last-resort guess at the window size;
                // enable_1m_context is an explicit statement about the window. If
                // both are set, honouring max_tokens would silently hand back a
                // tiny context (e.g. 32k) despite the 1M beta being active.
                if provider_config::is_1m_context_enabled(config, provider_name) {
                    // This flag also sends the `context-1m-2025-08-07` beta header,
                    // so local accounting must match the real API limit or we'd
                    // compact/trim long before we actually need to.
                    debug!(
                        "1M context beta enabled for provider={}, using 1000000",
                        provider_name
                    );
                    1_000_000
                } else if let Some(max_tokens) =
                    provider_config::get_max_tokens(config, provider_name)
                {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else {
                    200000
                }
            }
            "databricks" => {
                // Databricks models have varying context windows depending on the model
                if let Some(max_tokens) = provider_config::get_max_tokens(config, provider_name) {
                    warnings.push(format!(
                        "Context length falling back to max_tokens ({}) for provider={}",
                        max_tokens, provider_name
                    ));
                    max_tokens
                } else if model_name.contains("claude") {
                    200000 // Claude models on Databricks have large context windows
                } else if model_name.contains("llama") || model_name.contains("dbrx") {
                    32768 // DBRX supports 32k context
                } else {
                    16384 // Conservative default for other Databricks models
                }
            }
            "gemini" => {
                // Gemini models - use provider's context_window_size()
                if let Some(ctx_size) = provider.context_window_size() {
                    debug!(
                        "Using context window size {} from Gemini provider",
                        ctx_size
                    );
                    ctx_size
                } else {
                    1_000_000 // Default for Gemini models
                }
            }
            _ => config.agent.fallback_default_max_tokens as u32,
        };

        debug!(
            "Using context length: {} tokens for provider: {} (model: {})",
            context_length, provider_name, model_name
        );

        Ok(context_length)
    }

    pub fn get_provider_info(&self) -> Result<(String, String)> {
        let provider = self.providers.get(None)?;
        Ok((provider.name().to_string(), provider.model().to_string()))
    }

    /// Get the default LLM provider
    pub fn get_provider(&self) -> Result<&dyn g3_providers::LLMProvider> {
        self.providers.get(None)
    }

    /// Get the current session ID for this agent
    pub fn get_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Set the session ID (useful for testing)
    pub fn set_session_id(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// Set the working directory (useful for testing)
    pub fn set_working_dir(&mut self, working_dir: String) {
        self.working_dir = Some(working_dir);
    }

    // =========================================================================
    // RESEARCH MANAGEMENT
    // =========================================================================

    /// Inject completed research results into the conversation context.
    ///
    /// Returns the number of research results injected.
    pub fn inject_completed_research(&mut self) -> usize {
        let completed = self.pending_research_manager.take_completed();
        
        if completed.is_empty() {
            return 0;
        }
        
        for task in &completed {
            let message_content = match task.status {
                pending_research::ResearchStatus::Complete => {
                    format!(
                        "📋 **Research completed** (id: `{}`): {}\n\n{}",
                        task.id,
                        task.query,
                        task.result.as_deref().unwrap_or("No result available")
                    )
                }
                pending_research::ResearchStatus::Failed => {
                    format!(
                        "❌ **Research failed** (id: `{}`): {}\n\nError: {}",
                        task.id,
                        task.query,
                        task.result.as_deref().unwrap_or("Unknown error")
                    )
                }
                pending_research::ResearchStatus::Pending => continue, // Skip pending tasks
            };
            
            // Inject as a user message so the agent sees and responds to it
            let message = g3_providers::Message::new(g3_providers::MessageRole::User, message_content);
            self.context_window.add_message(message);
        }
        
        completed.len()
    }

    /// Subscribe to research completion notifications.
    ///
    /// Returns None if notifications are not enabled.
    pub fn subscribe_research_notifications(&self) -> Option<tokio::sync::broadcast::Receiver<pending_research::ResearchCompletionNotification>> {
        self.pending_research_manager.subscribe()
    }

    /// Enable research completion notifications and return a receiver.
    ///
    /// This replaces the internal research manager with one that sends notifications.
    /// Call this once during setup (e.g., in interactive mode) before any research tasks.
    /// Returns a receiver that will receive notifications when research tasks complete.
    pub fn enable_research_notifications(&mut self) -> tokio::sync::broadcast::Receiver<pending_research::ResearchCompletionNotification> {
        let (manager, rx) = pending_research::PendingResearchManager::with_notifications();
        self.pending_research_manager = manager;
        rx
    }

    /// Get a reference to the pending research manager.
    pub fn pending_research_manager(&self) -> &pending_research::PendingResearchManager {
        &self.pending_research_manager
    }

    // =========================================================================
    // MID-TURN USER INPUT
    // =========================================================================

    /// Drain queued mid-turn user messages and inject them as user turns.
    ///
    /// Returns the number of messages injected.
    ///
    /// Callers MUST invoke this only at the top of the streaming loop — see the
    /// call site for why any other position corrupts tool_use/tool_result
    /// pairing.
    pub fn inject_pending_input(&mut self) -> usize {
        let Some(session_id) = self.session_id.as_deref() else {
            // No session id means no session directory, so nowhere to queue to.
            return 0;
        };
        let inbox = crate::paths::get_inbox_dir(session_id);
        let queued = pending_input::drain(&inbox);
        if queued.is_empty() {
            return 0;
        }

        for input in &queued {
            // Marked so the model can tell an interjection from a fresh turn —
            // it arrives mid-task, and without the marker reads as though the
            // previous instruction had been restated.
            let content = format!(
                "💬 **User message sent while you were working** — read it now and \
                 adjust course if it changes the task; otherwise acknowledge and carry on:\n\n{}",
                input.text
            );
            let message =
                g3_providers::Message::new(g3_providers::MessageRole::User, content);
            self.context_window.add_message(message);
            // Console gets a one-line notice; the event stream gets the full
            // text so butler.app can render the real bubble.
            self.ui_writer
                .println(&format!("💬 queued message injected: {}", first_line(&input.text)));
            self.ui_writer.print_user_injected(&input.text);
        }

        queued.len()
    }

    // =========================================================================
    // TASK EXECUTION
    // =========================================================================

    pub async fn execute_task(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
    ) -> Result<TaskResult> {
        self.execute_task_with_options(description, language, false, false, false, None)
            .await
    }

    pub async fn execute_task_with_options(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        self.execute_task_with_timing(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            false,
            discovery_options,
        )
        .await
    }

    pub async fn execute_task_with_timing(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        show_timing: bool,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Create a cancellation token that never cancels for backward compatibility
        let cancellation_token = CancellationToken::new();
        self.execute_task_with_timing_cancellable(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
            discovery_options,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_task_with_timing_cancellable(
        &mut self,
        description: &str,
        _language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Execute the task directly without splitting
        self.execute_single_task(
            description,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
            discovery_options,
        )
        .await
    }

    async fn execute_single_task(
        &mut self,
        description: &str,
        _show_prompt: bool,
        _show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
        discovery_options: Option<DiscoveryOptions<'_>>,
    ) -> Result<TaskResult> {
        // Reset the JSON tool call filter state at the start of each new task
        // This prevents the filter from staying in suppression mode between user interactions
        self.ui_writer.reset_json_filter();

        // Validate that the system prompt is the first message (critical invariant)
        self.validate_system_prompt_is_first();

        // Generate session ID based on the initial prompt if this is a new session
        if self.session_id.is_none() {
            self.session_id = Some(self.generate_session_id(description));
        }

        // Add user message to context window
        let mut user_message = {
            let provider = self.providers.get(None)?;
            let content = description.to_string();

            // Apply cache control if provider supports it
            if let Some(cache_config) = self.get_provider_cache_control() {
                Message::with_cache_control_validated(
                    MessageRole::User,
                    content,
                    cache_config,
                    provider,
                )
            } else {
                Message::new(MessageRole::User, content)
            }
        };

        // Attach any pending images to this user message
        if !self.pending_images.is_empty() {
            user_message.images = std::mem::take(&mut self.pending_images);
        }

        self.context_window.add_message(user_message);

        // Execute fast-discovery tool calls if provided (immediately after user message)
        if let Some(ref options) = discovery_options {
            self.ui_writer
                .println("▶️  Playing back discovery commands...");
            // Store the working directory for subsequent tool calls in the streaming loop
            if let Some(path) = options.fast_start_path {
                self.working_dir = Some(path.to_string());
            }
            let provider = self.providers.get(None)?;
            let supports_cache = provider.supports_cache_control();
            let message_count = options.messages.len();

            for (idx, discovery_msg) in options.messages.iter().enumerate() {
                if let Ok(tool_call) = serde_json::from_str::<ToolCall>(&discovery_msg.content) {
                    self.add_message_to_context(discovery_msg.clone());
                    let result = self
                        .execute_tool_call_in_dir(&tool_call, options.fast_start_path)
                        .await
                        .unwrap_or_else(|e| format!("Error: {}", e));

                    // Add cache_control to the last user message if provider supports it (anthropic)
                    let is_last = idx == message_count - 1;
                    let result_message = if supports_cache
                        && is_last
                        && self.count_cache_controls_in_history() < 4
                    {
                        Message::with_cache_control(
                            MessageRole::User,
                            format!("Tool result: {}", result),
                            CacheControl::ephemeral(),
                        )
                    } else {
                        Message::new(MessageRole::User, format!("Tool result: {}", result))
                    };
                    self.add_message_to_context(result_message);
                }
            }
        }

        // Use the complete conversation history for the request
        let messages = self.context_window.conversation_history.clone();

        // Check if provider supports native tool calling and add tools if so
        let provider = self.providers.get(None)?;
        let provider_name = provider.name().to_string();
        let _has_native_tool_calling = provider.has_native_tool_calling();
        let _supports_cache_control = provider.supports_cache_control();
        let tools = if provider.has_native_tool_calling() {
            let tool_config = tool_definitions::ToolConfig::new(
                self.config.computer_control.enabled,
            );
            Some(self.get_tool_definitions_with_loaded_toolsets(tool_config))
        } else {
            None
        };
        let _ = provider; // Drop the provider reference to avoid borrowing issues

        // Get max_tokens from provider configuration with preflight validation
        // This ensures max_tokens > thinking.budget_tokens for Anthropic with extended thinking
        let initial_max_tokens = self.resolve_max_tokens(&provider_name);
        let max_tokens = Some(self.apply_max_tokens_fallback_sequence(
            &provider_name,
            initial_max_tokens,
            16000, // Hard-coded minimum for main API calls (higher than summary's 5000)
        ));

        let request = CompletionRequest {
            messages,
            max_tokens,
            temperature: Some(self.resolve_temperature(&provider_name)),
            stream: true, // Enable streaming
            tools,
            disable_thinking: false,
        };

        // Time the LLM call with cancellation support and streaming
        let llm_start = Instant::now();
        let result = tokio::select! {
            result = self.stream_completion(request, show_timing) => result,
            _ = cancellation_token.cancelled() => {
                // Save context window on cancellation
                self.save_context_window("cancelled");
                Err(anyhow::anyhow!("Operation cancelled by user"))
            }
        };

        let task_result = match result {
            Ok(result) => result,
            Err(e) => {
                // Save context window on error
                self.save_context_window("error");
                return Err(e);
            }
        };

        let response_content = task_result.response.clone();
        let _llm_duration = llm_start.elapsed();

        // Add assistant response to context window only if not empty
        // This prevents the "Skipping empty message" warning when only tools were executed
        // Also strip timing footer - it's display-only and shouldn't be in context
        let content_for_context = if let Some(timing_pos) = response_content.rfind("\n\n⏱️") {
            response_content[..timing_pos].to_string()
        } else {
            response_content.clone()
        };
        if !content_for_context.trim().is_empty() {
            let assistant_message = Message::new(MessageRole::Assistant, content_for_context);
            self.context_window.add_message(assistant_message);
        } else {
            debug!("Assistant response was empty (likely only tool execution), skipping message addition");
        }

        // Save context window at the end of successful interaction
        self.save_context_window("completed");

        // Check if we need to do 90% auto-compaction
        if self.pending_90_compaction {
            self.ui_writer.println("");
            self.ui_writer.print_g3_progress("compacting session");
            match self.force_compact().await {
                Ok(true) => self.ui_writer.print_g3_status("compacting session", "done"),
                Ok(false) => self.ui_writer.print_g3_status("compacting session", "failed"),
                Err(e) => {
                    self.ui_writer.print_g3_status("compacting session", &format!("error: {}", e));
                }
            }
            self.pending_90_compaction = false;
        }

        // Return the task result which already includes timing if needed
        Ok(task_result)
    }

    // =========================================================================
    // SESSION MANAGEMENT
    // =========================================================================

    /// Generate a session ID based on the initial prompt
    fn generate_session_id(&self, description: &str) -> String {
        session::generate_session_id(description, self.agent_name.as_deref())
    }

    /// Save the entire context window to a per-session file
    fn save_context_window(&self, status: &str) {
        if self.quiet {
            return;
        }
        session::save_context_window(self.session_id.as_deref(), &self.context_window, status);
    }

    /// Write context window summary to file
    /// Format: date&time, token_count, message_id, role, first_100_chars
    fn write_context_window_summary(&self) {
        if self.quiet {
            return;
        }
        if let Some(ref session_id) = self.session_id {
            session::write_context_window_summary(session_id, &self.context_window);
        }
    }

    pub fn get_context_window(&self) -> &ContextWindow {
        &self.context_window
    }

    /// Get a reference to the UI writer.
    pub fn ui_writer(&self) -> &W {
        &self.ui_writer
    }

    /// Add a message directly to the context window.
    /// Used for injecting discovery messages before the first LLM turn.
    pub fn add_message_to_context(&mut self, message: Message) {
        self.context_window.add_message(message);
    }

    /// Execute a tool call and return the result.
    /// This is a public wrapper around execute_tool for use by external callers
    /// like the planner's fast-discovery feature.
    pub async fn execute_tool_call(&mut self, tool_call: &ToolCall) -> Result<String> {
        self.execute_tool(tool_call).await
    }

    /// Execute a tool call with an optional working directory (for discovery commands)
    pub async fn execute_tool_call_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        self.execute_tool_in_dir(tool_call, working_dir).await
    }

    /// Log an error message to the session JSON file as the last message
    /// This is used in autonomous mode to record context length exceeded errors
    pub fn log_error_to_session(
        &self,
        error: &anyhow::Error,
        role: &str,
        forensic_context: Option<String>,
    ) {
        if self.quiet {
            return;
        }
        match &self.session_id {
            Some(id) => session::log_error_to_session(id, error, role, forensic_context),
            None => {
                error!("Cannot log error to session: no session ID");
            }
        }
    }

    // =========================================================================
    // CONTEXT WINDOW OPERATIONS
    // =========================================================================

    /// Manually trigger context compaction regardless of context window size
    /// Returns Ok(true) if compaction was successful, Ok(false) if it failed
    pub async fn force_compact(&mut self) -> Result<bool> {
        use crate::compaction::{perform_compaction, CompactionConfig};

        debug!("Manual compaction triggered");

        // Note: Status messages are now handled by the CLI layer

        let provider = self.providers.get(None)?;
        let provider_name = provider.name().to_string();
        let _ = provider; // Release borrow early

        // Get the latest user message to preserve it
        let latest_user_msg = self
            .context_window
            .conversation_history
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.content.clone());

        let compaction_config = CompactionConfig {
            provider_name: &provider_name,
            latest_user_msg,
        };

        let result = perform_compaction(
            &self.providers,
            &mut self.context_window,
            &self.config,
            compaction_config,
            &self.ui_writer,
            &mut self.thinning_events,
        )
        .await?;

        if result.success {
            // Note: Success message is now handled by the CLI layer
            self.compaction_events.push(result.chars_saved);
            Ok(true)
        } else {
            self.ui_writer.print_context_status(
                "⚠️ Unable to create summary. Please try again or start a new session.\n",
            );
            Ok(false)
        }
    }
    /// Manually trigger context thinning regardless of thresholds
    pub fn force_thin(&mut self) -> ThinResult {
        debug!("Manual context thinning triggered");
        self.do_thin_context()
    }

    /// Manually trigger context thinning for the ENTIRE context window
    /// Unlike force_thin which only processes the first third, this processes all messages
    pub fn force_thin_all(&mut self) -> ThinResult {
        debug!("Manual full context skinnifying triggered");
        self.do_thin_context_all()
    }

    /// Internal helper: thin context and track the event
    fn do_thin_context(&mut self) -> ThinResult {
        let result = self.context_window.thin_context(self.session_id.as_deref());
        self.thinning_events.push(result.chars_saved);
        result
    }

    /// Internal helper: thin all context and track the event
    fn do_thin_context_all(&mut self) -> ThinResult {
        let result = self
            .context_window
            .thin_context_all(self.session_id.as_deref());
        self.thinning_events.push(result.chars_saved);
        result
    }

    /// Ensure context window has capacity before streaming.
    /// Tries thinning first (cheap), then compaction (requires LLM call).
    /// Returns Ok(true) if request.messages was updated, Ok(false) if unchanged.
    async fn ensure_context_capacity(&mut self, request: &mut CompletionRequest) -> Result<bool> {
        if !self.context_window.should_compact() {
            return Ok(false);
        }

        // Try thinning first if at high capacity
        if self.context_window.percentage_used() > 90.0 && self.context_window.should_thin() {
            self.ui_writer.print_context_status(&format!(
                "\n🥒 Context window at {}%. Trying thinning first...",
                self.context_window.percentage_used() as u32
            ));

            let thin_result = self.do_thin_context();
            self.ui_writer.print_thin_result(&thin_result);

            if !self.context_window.should_compact() {
                self.ui_writer.print_g3_status("thinning", "resolved");
                return Ok(false);
            }
            self.ui_writer.print_g3_status("thinning", "insufficient");
            self.ui_writer.print_g3_progress("compacting session");
        }

        // Compaction still needed
        if !self.context_window.should_compact() {
            return Ok(false);
        }

        use crate::compaction::{perform_compaction, CompactionConfig};

        self.ui_writer.println("");
        self.ui_writer.print_g3_progress(
            &format!("compacting session ({}%)", self.context_window.percentage_used() as u32)
        );

        let provider_name = self.providers.get(None)?.name().to_string();
        let latest_user_msg = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.content.clone());

        let compaction_config = CompactionConfig {
            provider_name: &provider_name,
            latest_user_msg,
        };

        let result = perform_compaction(
            &self.providers,
            &mut self.context_window,
            &self.config,
            compaction_config,
            &self.ui_writer,
            &mut self.thinning_events,
        )
        .await?;

        if result.success {
            self.ui_writer.print_g3_status("compacting session", "done");
            self.compaction_events.push(result.chars_saved);
            request.messages = self.context_window.conversation_history.clone();
            return Ok(true);
        }

        self.ui_writer.print_g3_status("compacting session", "failed");
        Err(anyhow::anyhow!(
            "Context window at capacity and compaction failed. Please start a new session."
        ))
    }

    /// Check if a tool call is a duplicate of the last tool call in the previous assistant message.
    /// Returns Some("DUP IN MSG") if it's a duplicate, None otherwise.
    fn check_duplicate_in_previous_message(&self, tool_call: &ToolCall, history_cutoff: usize) -> Option<String> {
        // Find the most recent assistant message, but only look at messages that
        // existed before the current streaming iteration started. This prevents
        // tool calls within the same response from being marked as DUP IN MSG
        // against messages added during the current iteration's tool execution.
        for msg in self.context_window.conversation_history[..history_cutoff].iter().rev() {
            if !matches!(msg.role, MessageRole::Assistant) {
                continue;
            }

            // Check structured tool_calls first (native tool calling)
            if !msg.tool_calls.is_empty() {
                if let Some(last_tc) = msg.tool_calls.last() {
                    // When both tool calls have non-empty IDs from native providers
                    // (e.g. Anthropic "toolu_*"), each API invocation gets a unique ID.
                    // Different IDs mean distinct invocations — not duplicates.
                    // This is critical for polling tools like research_status that are
                    // called repeatedly with identical arguments across turns.
                    if !last_tc.id.is_empty() && !tool_call.id.is_empty() {
                        if last_tc.id == tool_call.id {
                            return Some("DUP IN MSG".to_string());
                        }
                        // Different IDs = different invocations, not a duplicate
                    } else {
                        // Fallback for JSON-based tool calls (embedded models) where
                        // IDs are auto-generated and not meaningful for dedup.
                        let prev = ToolCall {
                            tool: last_tc.name.clone(),
                            args: last_tc.input.clone(),
                            id: last_tc.id.clone(),
                        };
                        if streaming::are_tool_calls_duplicate(&prev, tool_call) {
                            return Some("DUP IN MSG".to_string());
                        }
                    }
                }
                // Only check the most recent assistant message
                break;
            }

            let content = &msg.content;

            // Look for the last occurrence of a tool call pattern
            let last_tool_start = content
                .rfind(r#"{"tool""#)
                .or_else(|| content.rfind(r#"{ "tool""#))?;

            // Find the end of this JSON object
            let end_offset =
                StreamingToolParser::find_complete_json_object_end(&content[last_tool_start..])?;
            let end_idx = last_tool_start + end_offset + 1;
            let tool_json = &content[last_tool_start..end_idx];

            // Check if there's any non-whitespace text after this tool call
            let text_after = content[end_idx..].trim();
            if !text_after.is_empty() {
                // There's text after the tool call, so it's not a trailing duplicate
                return None;
            }

            // Parse and compare the tool call
            if let Ok(prev_tool) = serde_json::from_str::<ToolCall>(tool_json) {
                if streaming::are_tool_calls_duplicate(&prev_tool, tool_call) {
                    return Some("DUP IN MSG".to_string());
                }
            }

            // Only check the most recent assistant message
            break;
        }

        None
    }

    /// Reload README.md and AGENTS.md and replace the first system message
    /// Returns Ok(true) if README was found and reloaded, Ok(false) if no README was present initially
    pub fn reload_readme(&mut self) -> Result<bool> {
        debug!("Manual README reload triggered");

        // Check if the second message in conversation history is a system message with README content
        // (The first message should always be the system prompt)
        let has_readme = self
            .context_window
            .conversation_history
            .get(1) // Check the SECOND message (index 1)
            .map(|m| {
                matches!(m.role, MessageRole::System)
                    && (m.content.contains("Project README")
                        || m.content.contains("Agent Configuration"))
            })
            .unwrap_or(false);

        // Validate that the system prompt is still first
        self.validate_system_prompt_is_first();

        if !has_readme {
            return Ok(false);
        }

        // Try to load README.md and AGENTS.md
        let mut combined_content = String::new();
        let mut found_any = false;

        if let Ok(agents_content) = std::fs::read_to_string("AGENTS.md") {
            combined_content.push_str("# Agent Configuration\n\n");
            combined_content.push_str(&agents_content);
            combined_content.push_str("\n\n");
            found_any = true;
        }

        if let Ok(readme_content) = std::fs::read_to_string("README.md") {
            combined_content.push_str("# Project README\n\n");
            combined_content.push_str(&readme_content);
            found_any = true;
        }

        if found_any {
            // Replace the second message (README) with the new content
            if let Some(first_msg) = self.context_window.conversation_history.get_mut(1) {
                first_msg.content = combined_content;
                debug!("README content reloaded successfully");
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Set or clear project content in the system message.
    /// Project content is appended to the second system message (README/AGENTS content)
    /// so it survives compaction and dehydration.
    ///
    /// Pass `Some(content)` to set project content, `None` to clear it.
    /// Returns true if the operation succeeded.
    pub fn set_project_content(&mut self, content: Option<String>) -> bool {
        // The second message (index 1) should be the README/AGENTS system message
        if self.context_window.conversation_history.len() < 2 {
            return false;
        }

        let second_msg = &mut self.context_window.conversation_history[1];
        if !matches!(second_msg.role, MessageRole::System) {
            return false;
        }

        // Remove any existing project content first
        if let Some(start_idx) = second_msg.content.find("\n\n=== PROJECT INSTRUCTIONS ===") {
            second_msg.content.truncate(start_idx);
        } else if let Some(start_idx) = second_msg.content.find("\n\n=== ACTIVE PROJECT:") {
            second_msg.content.truncate(start_idx);
        }

        // Add new project content if provided
        if let Some(project_content) = content {
            second_msg.content.push_str("\n\n");
            second_msg.content.push_str(&project_content);
        }

        true
    }

    /// Clear project content from the system message.
    /// This is equivalent to calling `set_project_content(None)`.
    pub fn clear_project_content(&mut self) -> bool {
        self.set_project_content(None)
    }

    /// Check if there is currently project content loaded.
    pub fn has_project_content(&self) -> bool {
        self.context_window.conversation_history.get(1)
            .map(|m| m.content.contains("=== ACTIVE PROJECT:"))
            .unwrap_or(false)
    }

    /// Get detailed context statistics
    pub fn get_stats(&self) -> String {
        use crate::stats::AgentStatsSnapshot;

        let snapshot = AgentStatsSnapshot {
            context_window: &self.context_window,
            thinning_events: &self.thinning_events,
            compaction_events: &self.compaction_events,
            first_token_times: &self.first_token_times,
            tool_call_metrics: &self.tool_call_metrics,
            provider_info: self.get_provider_info().ok(),
            cache_stats: &self.cache_stats,
        };

        snapshot.format()
    }

    pub fn get_tool_call_metrics(&self) -> &Vec<(String, Duration, bool)> {
        &self.tool_call_metrics
    }

    pub fn get_config(&self) -> &Config {
        &self.config
    }

    pub fn set_requirements_sha(&mut self, sha: String) {
        self.requirements_sha = Some(sha);
    }

    /// Save a session continuation artifact
    /// Save session continuation for potential resumption
    pub fn save_session_continuation(&self, summary: Option<String>) {
        use crate::session_continuation::{save_continuation, SessionContinuation};

        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                debug!("No session ID, skipping continuation save");
                return;
            }
        };

        // Get the session log path (now in .g3/sessions/<session_id>/session.json)
        let session_log_path = get_session_file(&session_id);

        // Get current TODO content - try session-specific path first, then workspace path
        let session_todo_path = crate::paths::get_session_todo_path(&session_id);
        let todo_snapshot = if session_todo_path.exists() {
            std::fs::read_to_string(&session_todo_path).ok()
        } else {
            // Fall back to workspace TODO path for backwards compatibility
            std::fs::read_to_string(get_todo_path()).ok()
        };

        // Get working directory
        let working_directory = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Get description from first user message (strip "Task: " prefix if present)
        let description = self
            .context_window
            .conversation_history
            .iter()
            .find(|m| matches!(m.role, g3_providers::MessageRole::User))
            .map(|m| {
                let content = m.content.strip_prefix("Task: ").unwrap_or(&m.content);
                // Truncate to ~60 chars for display, ending at word boundary
                truncate_to_word_boundary(content, 60)
            });

        let continuation = SessionContinuation::new(
            self.is_agent_mode,
            self.agent_name.clone(),
            session_id,
            description,
            summary,
            session_log_path.to_string_lossy().to_string(),
            self.context_window.percentage_used(),
            todo_snapshot,
            working_directory,
        );

        if let Err(e) = save_continuation(&continuation) {
            error!("Failed to save session continuation: {}", e);
        } else {
            debug!("Saved session continuation artifact");
        }
    }

    /// Set agent mode information for session tracking
    /// Called when running with --agent flag to enable agent-specific session resume
    pub fn set_agent_mode(&mut self, agent_name: &str) {
        self.is_agent_mode = true;
        self.agent_name = Some(agent_name.to_string());
        debug!("Agent mode enabled for agent: {}", agent_name);
    }

    /// Enable auto-memory reminders after turns with tool calls
    pub fn set_auto_memory(&mut self, enabled: bool) {
        self.auto_memory = enabled;
        debug!(
            "Auto-memory reminders: {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }

    /// Enable or disable aggressive context dehydration (ACD)
    pub fn set_acd_enabled(&mut self, enabled: bool) {
        self.acd_enabled = enabled;
        debug!(
            "ACD (aggressive context dehydration): {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }

    /// Enable or disable plan mode (blocks file changes without approved plan)
    pub fn set_plan_mode(&mut self, enabled: bool, working_dir: Option<&str>) {
        self.in_plan_mode = enabled;
        if enabled {
            // Capture current dirty files as baseline so the approval gate
            // won't block on files that were already dirty before plan mode.
            self.baseline_dirty_files = get_dirty_files(working_dir);
        } else {
            self.baseline_dirty_files.clear();
        }
    }

    /// Check if plan mode is active
    pub fn is_plan_mode(&self) -> bool {
        self.in_plan_mode
    }

    /// Check if the current plan is in a terminal state (all items done or blocked).
    ///
    /// Returns true if:
    /// - A plan exists AND all items are in terminal state (done or blocked)
    ///
    /// Returns false if:
    /// - No session_id is set
    /// - No plan exists for the session
    /// - Plan has items that are not terminal (todo or doing)
    pub fn is_plan_terminal(&self) -> bool {
        let Some(session_id) = &self.session_id else {
            return false;
        };
        read_plan(session_id).ok().flatten().map_or(false, |plan| plan.is_complete())
    }

    // =========================================================================
    // STREAMING & LLM INTERACTION
    // =========================================================================

    /// Build the final response and prepare for return.
    ///
    /// This is the single canonical path for completing a streaming turn:
    /// 1. Finish streaming markdown
    /// 2. Save context window
    /// 3. Add timing footer (if requested)
    /// 4. Dehydrate context (if ACD enabled)
    /// 5. Build TaskResult
    fn finalize_streaming_turn(
        &mut self,
        full_response: String,
        show_timing: bool,
        stream_start: Instant,
        first_token_time: Option<Duration>,
        turn_accumulated_usage: &Option<g3_providers::Usage>,
    ) -> TaskResult {
        self.ui_writer.finish_streaming_markdown();
        self.save_context_window("completed");

        let final_response = if show_timing {
            let ttft = first_token_time.unwrap_or_else(|| stream_start.elapsed());
            let turn_tokens = turn_accumulated_usage.as_ref().map(|u| u.total_tokens);
            let timing_footer = streaming::format_timing_footer(
                stream_start.elapsed(),
                ttft,
                turn_tokens,
                self.context_window.percentage_used(),
            );
            format!("{}\n\n{}", full_response, timing_footer)
        } else {
            full_response
        };

        self.dehydrate_context();
        TaskResult::new(final_response, self.context_window.clone())
    }

    /// Perform ACD dehydration - save current conversation state to a fragment.
    /// Called at the end of each turn when ACD is enabled.
    ///
    /// This saves all non-system messages (except the final assistant response)
    /// to a fragment, then replaces them with a compact stub. The final assistant
    /// response is preserved as the turn summary after the stub.
    ///
    /// in the context with a compact stub. The agent's final response (summary)
    /// is preserved after the stub.
    fn dehydrate_context(&mut self) {
        if !self.acd_enabled {
            return;
        }

        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                debug!("ACD: No session_id, skipping dehydration");
                return;
            }
        };

        // Find the index of the last dehydration stub (marks the end of previously dehydrated content)
        // We only want to dehydrate messages AFTER the last stub+summary pair
        let last_stub_index = self
            .context_window
            .conversation_history
            .iter()
            .rposition(|m| m.is_dehydrated_stub());

        // Start index for messages to dehydrate:
        // - If there's a previous stub, start after the stub AND its following summary (stub + 2)
        // - Otherwise, start from the beginning (index 0)
        let dehydrate_start = match last_stub_index {
            Some(idx) => idx + 2, // Skip the stub and the summary that follows it
            None => 0,
        };

        // Get the preceding fragment ID (if any)
        let preceding_id = crate::acd::get_latest_fragment_id(&session_id)
            .ok()
            .flatten();

        // Extract only NEW non-system messages to dehydrate (after the last stub+summary)
        let messages_to_dehydrate: Vec<_> = self
            .context_window
            .conversation_history
            .iter()
            .enumerate()
            .filter(|(idx, m)| {
                *idx >= dehydrate_start && !matches!(m.role, g3_providers::MessageRole::System)
            })
            .map(|(_, m)| m.clone())
            .collect();

        if messages_to_dehydrate.is_empty() {
            return;
        }

        // Extract the last assistant message as the turn summary
        // This is the actual LLM response, not the timing footer passed in final_response
        let turn_summary: Option<String> = messages_to_dehydrate
            .iter()
            .rev()
            .find(|m| matches!(m.role, g3_providers::MessageRole::Assistant))
            .map(|m| m.content.clone());

        // Use extracted summary, falling back to final_response only if no assistant message found
        let summary_content = turn_summary.unwrap_or_default();

        // Create the fragment and generate stub
        let fragment = crate::acd::Fragment::new(messages_to_dehydrate, preceding_id);
        let stub = fragment.generate_stub();

        if let Err(e) = fragment.save(&session_id) {
            warn!("Failed to save ACD fragment: {}", e);
            return; // Don't modify context if save failed
        }

        // Now replace the context: keep system messages + previous stubs/summaries, add new stub, add new summary
        // Extract messages to keep: system messages + everything up to (but not including) dehydrate_start
        let messages_to_keep: Vec<_> = self
            .context_window
            .conversation_history
            .iter()
            .enumerate()
            .filter(|(idx, m)| {
                // Keep all system messages OR keep previous stub+summary pairs
                matches!(m.role, g3_providers::MessageRole::System) || *idx < dehydrate_start
            })
            .map(|(_, m)| m.clone())
            .collect();

        // Clear and rebuild context
        self.context_window.conversation_history.clear();

        // Add back kept messages (system + previous stubs/summaries)
        for msg in messages_to_keep {
            self.context_window.conversation_history.push(msg);
        }

        // Add the stub as a user message (so LLM sees it as context)
        let stub_msg = g3_providers::Message::with_kind(
            g3_providers::MessageRole::User,
            stub,
            g3_providers::MessageKind::DehydratedStub,
        );
        self.context_window.conversation_history.push(stub_msg);

        // Add the final response as assistant message (the summary)
        if !summary_content.trim().is_empty() {
            let summary_msg = g3_providers::Message::with_kind(
                g3_providers::MessageRole::Assistant,
                summary_content,
                g3_providers::MessageKind::Summary,
            );
            self.context_window.conversation_history.push(summary_msg);
        }

        // Recalculate token usage
        self.context_window.recalculate_tokens();
    }

    /// Send an auto-memory reminder to the LLM if tools were called during the turn.
    /// This prompts the LLM to call the `remember` tool if it discovered any key code locations.
    /// Returns true if a reminder was sent and processed.
    pub async fn send_auto_memory_reminder(&mut self) -> Result<bool> {
        if !self.auto_memory {
            return Ok(false);
        }

        // Check if any tools were called this turn
        if self.tool_calls_this_turn.is_empty() {
            debug!("Auto-memory: No tools called, skipping reminder");
            self.ui_writer.print_context_status(
                "📝 Auto-memory: No tools called this turn, skipping reminder.\n",
            );
            return Ok(false);
        }

        // Check if remember was already called this turn - no need to remind
        if self.tool_calls_this_turn.iter().any(|t| t == "remember") {
            debug!("Auto-memory: 'remember' was already called this turn, skipping reminder");
            self.ui_writer.print_context_status(
                "\n📝 Auto-memory: 'remember' already called, skipping reminder.\n",
            );
            self.tool_calls_this_turn.clear();
            return Ok(false);
        }

        // Take the tools list and reset for next turn
        let tools_called = std::mem::take(&mut self.tool_calls_this_turn);

        debug!(
            "Auto-memory: Sending reminder to LLM ({} tools called this turn: {:?})",
            tools_called.len(),
            tools_called
        );
        // IMPORTANT: The message MUST end with a newline so the LLM's response starts on a new line.
        // The JSON filter only suppresses tool calls that appear at line boundaries (after newline).
        // Without the trailing newline, tool call JSON like `{"tool": "remember", ...}` would
        // appear on the same line as "Memory checkpoint:" and leak through to the UI.
        // See test: test_tool_call_not_at_line_start_passes_through in filter_json.rs
        self.ui_writer.print_context_status("\nMemory checkpoint:\n");

        // Reset JSON filter state so it starts fresh for this response
        self.ui_writer.reset_json_filter();

        let reminder = r#"MEMORY CHECKPOINT: If you discovered code locations worth remembering, call `remember` now.

Use this rich format:

### Feature Name
Brief description of what this feature/subsystem does.

- `path/to/file.rs`
  - `FunctionName()` [1200..1450] - what it does, key params/return
  - `StructName` [500..650] - purpose, key fields
  - `related_function()` - how it connects

### Pattern Name
When to use this pattern and why.

1. First step
2. Second step
3. Key gotcha or tip

Example of a good entry:

### Session Continuation
Save/restore session state across g3 invocations using symlink-based approach.

- `crates/g3-core/src/session_continuation.rs`
  - `SessionContinuation` [850..2100] - artifact struct with session state, TODO snapshot, context %
  - `save_continuation()` [5765..7200] - saves to `.g3/sessions/<id>/latest.json`, updates symlink
  - `load_continuation()` [7250..8900] - follows `.g3/session` symlink to restore
  - `find_incomplete_agent_session()` [10500..13200] - finds sessions with incomplete TODOs for agent resume

Skip if nothing new. Be brief."#;

        // Add the reminder as a user message and get a response
        self.context_window
            .add_message(Message::new(MessageRole::User, reminder.to_string()));

        // Build the completion request
        let messages = self.context_window.conversation_history.clone();

        // Get provider and tools
        let provider = self.providers.get(None)?;
        let provider_name = provider.name().to_string();
        let tools = if provider.has_native_tool_calling() {
            let tool_config = tool_definitions::ToolConfig::new(
                self.config.computer_control.enabled,
            );
            Some(self.get_tool_definitions_with_loaded_toolsets(tool_config))
        } else {
            None
        };
        let _ = provider; // Drop the provider reference

        let max_tokens = Some(self.resolve_max_tokens(&provider_name));

        let request = CompletionRequest {
            messages,
            max_tokens,
            temperature: Some(self.resolve_temperature(&provider_name)),
            stream: true,
            tools,
            disable_thinking: true, // Keep it brief
        };

        // Execute the reminder turn (show_timing = false to keep it quiet)
        self.stream_completion_with_tools(request, false).await?;

        Ok(true)
    }

    /// Initialize session ID manually (primarily for testing).
    /// This allows tests to verify session ID generation without calling execute_task,
    /// which would require an LLM provider.
    pub fn init_session_id_for_test(&mut self, description: &str) {
        if self.session_id.is_none() {
            self.session_id = Some(self.generate_session_id(description));
        }
    }

    /// Clear session state and continuation artifacts (for /clear command)
    pub fn clear_session(&mut self) {
        use crate::session_continuation::clear_continuation;

        // Clear the context window (keep system prompt)
        self.context_window.clear_conversation();

        // Clear continuation artifacts
        if let Err(e) = clear_continuation() {
            error!("Failed to clear continuation artifacts: {}", e);
        }

        debug!("Session cleared");
    }

    /// Restore session from a continuation artifact
    /// Returns true if full context was restored, false if only summary was used
    pub fn restore_from_continuation(
        &mut self,
        continuation: &crate::session_continuation::SessionContinuation,
    ) -> Result<bool> {
        use std::path::PathBuf;

        let session_log_path = PathBuf::from(&continuation.session_log_path);

        // If context < 80%, try to restore full context
        if continuation.can_restore_full_context() && session_log_path.exists() {
            // Load the session log
            let json = std::fs::read_to_string(&session_log_path)?;
            let session_data: serde_json::Value = serde_json::from_str(&json)?;

            // Extract conversation history
            if let Some(context_window) = session_data.get("context_window") {
                if let Some(history) = context_window.get("conversation_history") {
                    if let Some(messages) = history.as_array() {
                        // Clear current conversation (keep system messages)
                        self.context_window.clear_conversation();

                        // Restore messages from session log (skip system messages as they're preserved)
                        for msg in messages {
                            let role_str =
                                msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                            // Skip system messages — already preserved on the agent.
                            if role_str == "system" {
                                continue;
                            }

                            // Prefer full serde deserialization so tool_calls and
                            // tool_result_id (and any future Message fields) survive
                            // the save→resume roundtrip. Fall back to a minimal
                            // reconstruction only for legacy/malformed entries.
                            let restored: Message =
                                match serde_json::from_value::<Message>(msg.clone()) {
                                    Ok(m) => m,
                                    Err(e) => {
                                        debug!(
                                            "Message deserialize failed ({}); falling back to minimal reconstruction",
                                            e
                                        );
                                        let content = msg
                                            .get("content")
                                            .and_then(|c| c.as_str())
                                            .unwrap_or("");
                                        let role = match role_str {
                                            "assistant" => MessageRole::Assistant,
                                            _ => MessageRole::User,
                                        };
                                        Message {
                                            role,
                                            id: String::new(),
                                            images: Vec::new(),
                                            content: content.to_string(),
                                            kind: g3_providers::MessageKind::Regular,
                                            cache_control: None,
                                            tool_calls: Vec::new(),
                                            tool_result_id: None,
                                        }
                                    }
                                };

                            self.context_window.add_message(restored);
                        }

                        // An eagerly-written snapshot can end on an assistant
                        // message whose tool calls were never answered (the
                        // "error"/"cancelled" saves fire wherever the failure
                        // happened, including mid-dispatch). Resuming that
                        // shape makes the very next API request invalid —
                        // fatally so on every provider except Anthropic, which
                        // is alone in stripping orphans defensively. Trim here,
                        // at the one point every restore path funnels through.
                        let trimmed = crate::session::trim_unanswered_tool_calls(
                            &mut self.context_window.conversation_history,
                        );
                        if trimmed > 0 {
                            debug!(
                                "Dropped {} incomplete trailing message(s) from resumed session {}",
                                trimmed, continuation.session_id
                            );
                            // Trimming bypasses add_message(), which is what
                            // maintains used_tokens. Left stale, the count
                            // would overstate usage for the rest of the
                            // session and push it toward premature compaction.
                            self.context_window.recalculate_tokens();
                        }

                        // Continue IN PLACE: adopt the resumed session's id so
                        // this turn rewrites that session dir instead of
                        // minting a new one. Without this, execute_task() sees
                        // session_id == None and generates a fresh id, so every
                        // resume forked a new dir holding a full copy of the
                        // history — one conversation became N directories.
                        //
                        // Only safe on the FULL-restore path: the whole
                        // transcript is now in memory, so rewriting the file is
                        // lossless. The summary-only fallback below must NOT do
                        // this (see the comment there).
                        self.session_id = Some(continuation.session_id.clone());

                        debug!(
                            "Restored full context from session log; continuing in place as {}",
                            continuation.session_id
                        );
                        return Ok(true);
                    }
                }
            }
        }

        // Fall back to using session summary + TODO.
        //
        // DELIBERATELY does NOT adopt continuation.session_id. We only hold a
        // summary of the conversation, not the conversation, so continuing in
        // place would have this turn's save_context_window() overwrite a
        // complete transcript with a summary plus a couple of new turns —
        // silent, irreversible history loss. Leaving session_id as None means
        // execute_task() mints a fresh dir and the original stays intact.
        // This path triggers when context >= 80% (see can_restore_full_context)
        // or when the session log is missing/unreadable.
        let mut context_msg = String::new();
        if let Some(ref summary) = continuation.summary {
            context_msg.push_str(&format!("Previous session summary:\n{}\n\n", summary));
        }
        if let Some(ref todo) = continuation.todo_snapshot {
            context_msg.push_str(&format!("Current TODO state:\n{}\n", todo));
        }

        if !context_msg.is_empty() {
            self.context_window.add_message(Message {
                role: MessageRole::User,
                id: String::new(),
                images: Vec::new(),
                content: format!("[Session Resumed]\n\n{}", context_msg),
                kind: g3_providers::MessageKind::Regular,
                cache_control: None,
                tool_calls: Vec::new(),
                tool_result_id: None,
            });
        }

        debug!(
            "Restored session {} from summary only; starting a NEW session dir to \
             avoid overwriting the full transcript",
            continuation.session_id
        );
        Ok(false)
    }

    /// Switch to a different session, saving the current one first.
    /// This discards the current in-memory state and loads the new session.
    pub fn switch_to_session(
        &mut self,
        continuation: &crate::session_continuation::SessionContinuation,
    ) -> Result<bool> {
        // Save current session first (so it can be resumed later)
        self.save_session_continuation(None);

        // Reset session-specific metrics
        self.thinning_events.clear();
        self.compaction_events.clear();
        self.first_token_times.clear();
        self.tool_call_metrics.clear();
        self.tool_call_count = 0;
        self.pending_90_compaction = false;

        // Update session ID to the new session
        self.session_id = Some(continuation.session_id.clone());

        // Update agent mode info from continuation
        self.is_agent_mode = continuation.is_agent_mode;
        self.agent_name = continuation.agent_name.clone();

        // Load TODO content from the new session if available
        if let Some(ref todo) = continuation.todo_snapshot {
            // Use blocking write since we're in a sync context
            if let Ok(mut guard) = self.todo_content.try_write() {
                *guard = todo.clone();
            }
        }

        // Restore context from the continuation
        self.restore_from_continuation(continuation)
    }

    async fn stream_completion(
        &mut self,
        request: CompletionRequest,
        show_timing: bool,
    ) -> Result<TaskResult> {
        self.stream_completion_with_tools(request, show_timing)
            .await
    }

    /// Create tool definitions for native tool calling providers

    /// Helper method to stream with retry logic
    async fn stream_with_retry(
        &self,
        request: &CompletionRequest,
        error_context: &error_handling::ErrorContext,
    ) -> Result<g3_providers::CompletionStream> {
        use crate::error_handling::{calculate_retry_delay, classify_error, ErrorType};

        let mut attempt = 0;
        let max_attempts = if self.is_autonomous {
            self.config.agent.autonomous_max_retry_attempts
        } else {
            self.config.agent.max_retry_attempts
        };

        loop {
            attempt += 1;
            let provider = self.providers.get(None)?;

            match provider.stream(request.clone()).await {
                Ok(stream) => {
                    if attempt > 1 {
                        debug!("Stream started successfully after {} attempts", attempt);
                    }
                    debug!("Stream started successfully");
                    debug!(
                        "Request had {} messages, tools={}, max_tokens={:?}",
                        request.messages.len(),
                        request.tools.is_some(),
                        request.max_tokens
                    );
                    return Ok(stream);
                }
                Err(e) if attempt < max_attempts => {
                    if matches!(classify_error(&e), ErrorType::Recoverable(_)) {
                        let delay = calculate_retry_delay(attempt, self.is_autonomous);
                        warn!(
                            "Recoverable error on attempt {}/{}: {}. Retrying in {:?}...",
                            attempt, max_attempts, e, delay
                        );
                        tokio::time::sleep(delay).await;
                    } else {
                        error_context.clone().log_error(&e);
                        return Err(e);
                    }
                }
                Err(e) => {
                    error_context.clone().log_error(&e);
                    return Err(e);
                }
            }
        }
    }

    async fn stream_completion_with_tools(
        &mut self,
        mut request: CompletionRequest,
        show_timing: bool,
    ) -> Result<TaskResult> {

        // =========================================================================
        // STREAMING COMPLETION WITH TOOL EXECUTION
        // =========================================================================
        // This function orchestrates the streaming LLM response loop:
        //   1. Pre-loop: Check context capacity, compact/thin if needed
        //   2. Main loop: Stream chunks, detect tool calls, execute tools
        //   3. Auto-continue: Re-prompt LLM if tools executed or response truncated
        //   4. Post-loop: Finalize response, save context, return result
        // =========================================================================

        use crate::error_handling::ErrorContext;
        use tokio_stream::StreamExt;

        debug!("Starting stream_completion_with_tools");

        // --- State Initialization ---
        // Note: Session-level duplicate tracking was removed - we only prevent sequential duplicates (DUP IN CHUNK, DUP IN MSG)
        let mut state = streaming::StreamingState::new();

        // --- Phase 1: Pre-loop Context Capacity Check ---
        self.ensure_context_capacity(&mut request).await?;

        // --- Phase 2: Main Streaming Loop ---
        loop {
            state.iteration_count += 1;
            debug!("Starting iteration {}", state.iteration_count);
            if state.iteration_count > streaming::MAX_ITERATIONS {
                warn!("Maximum iterations reached, stopping stream");
                break;
            }

            // Add a small delay between iterations to prevent "model busy" errors
            if state.iteration_count > 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }

            // Inject any completed research results into the context
            let injected_count = self.inject_completed_research();
            if injected_count > 0 {
                debug!("Injected {} completed research result(s) into context", injected_count);
                self.ui_writer.println(&format!("📋 {} research result(s) ready and injected into context", injected_count));
            }

            // Inject any user messages queued mid-turn by an external process
            // (butler.app writes them into .g3/sessions/<id>/inbox/).
            //
            // ⚠️ THIS IS THE ONLY SAFE PLACE TO DO THIS. The provider strips
            // orphaned tool_use blocks (see anthropic.rs `sanitize`): if an
            // assistant message holds a tool_use and the *immediately next*
            // user message carries no matching tool_result, the tool call is
            // deleted from history. Injecting inside the tool-dispatch block
            // would therefore silently corrupt the conversation. Here at the
            // loop top, every tool result for the previous iteration has
            // already been appended, so a plain user message is safe.
            let injected_input = self.inject_pending_input();
            if injected_input > 0 {
                debug!("Injected {} queued user message(s) into context", injected_input);
            }

            // Both injections above mutate context_window. `request.messages`
            // is only refreshed from it inside the tool-dispatch block
            // (~L2734), so on an iteration that injects without dispatching a
            // tool the new messages would never reach the provider. Refresh
            // unconditionally when anything was injected.
            if injected_count > 0 || injected_input > 0 {
                request.messages = self.context_window.conversation_history.clone();
            }

            // Get provider info for logging, then drop it to avoid borrow issues
            let (provider_name, provider_model) = {
                let provider = self.providers.get(None)?;
                (provider.name().to_string(), provider.model().to_string())
            };
            debug!("Got provider: {}", provider_name);

            // Create error context for detailed logging
            let last_prompt = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, MessageRole::User))
                .map(|m| m.content.clone())
                .unwrap_or_else(|| "No user message found".to_string());

            let error_context = ErrorContext::new(
                "stream_completion".to_string(),
                provider_name.clone(),
                provider_model.clone(),
                last_prompt,
                self.session_id.clone(),
                self.context_window.used_tokens,
                self.quiet,
            )
            .with_request(
                serde_json::to_string(&request)
                    .unwrap_or_else(|_| "Failed to serialize request".to_string()),
            );

            // Log initial request details
            debug!("Starting stream with provider={}, model={}, messages={}, tools={}, max_tokens={:?}",
                provider_name,
                provider_model,
                request.messages.len(),
                request.tools.is_some(),
                request.max_tokens
            );

            // Try to get stream with retry logic
            let mut stream = match self.stream_with_retry(&request, &error_context).await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to start stream: {}", e);
                    // Additional retry for "busy" errors on subsequent iterations
                    if state.iteration_count > 1 && e.to_string().contains("busy") {
                        warn!(
                            "Model busy on iteration {}, attempting one more retry in 500ms",
                            state.iteration_count
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                        match self.stream_with_retry(&request, &error_context).await {
                            Ok(s) => s,
                            Err(e2) => {
                                error!("Failed to start stream after retry: {}", e2);
                                error_context.clone().log_error(&e2);
                                return Err(e2);
                            }
                        }
                    } else {
                        return Err(e);
                    }
                }
            };

            // Write context window summary every time we send messages to LLM
            self.write_context_window_summary();

            // Persist the FULL transcript too, not just the human-readable
            // summary. Without this, session.json was written only at the end
            // of a turn (see the "completed"/"error"/"cancelled" calls in
            // process_message), so a turn killed mid-flight — wall-clock kill,
            // crash, machine sleep, or the server it runs under being
            // restarted — left a session dir holding context_summary.txt and
            // NOTHING ELSE. That dir is unreadable and unresumable: the whole
            // conversation is gone. Measured on one real workspace: 10 of 219
            // butler session dirs had no session.json, the largest having lost
            // 210 messages.
            //
            // THIS IS THE ONLY PLACE THE SNAPSHOT IS GUARANTEED VALID.
            // Anthropic requires every tool_use block to be answered by a
            // tool_result in the very next message. At the top of the loop that
            // invariant always holds, because every tool result for the
            // previous iteration has already been appended (the same reason
            // inject_pending_input is only safe here). Saving from inside the
            // tool-dispatch block would persist an assistant message whose
            // tool_use has no answer yet, and resuming that is an API error.
            //
            // Status is "running" rather than "completed": a reader must be
            // able to tell a turn in progress from one that finished. The
            // end-of-turn save overwrites it with the real terminal status.
            self.save_context_window("running");

            // Create fresh iteration state for this streaming iteration
            let mut iter = streaming::IterationState::new();

            // Snapshot history length before this iteration. Used by DUP IN MSG
            // detection to avoid comparing against messages added during this
            // iteration's own tool executions (which would false-positive on
            // multi-tool responses where the model issues the same tool twice).
            let history_len_before_iteration = self.context_window.conversation_history.len();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Notify UI about SSE received (including pings)
                        self.ui_writer.notify_sse_received();

                        // Capture usage data if available
                        if let Some(ref usage) = chunk.usage {
                            iter.accumulated_usage = Some(usage.clone());
                            state.turn_accumulated_usage = Some(usage.clone());
                            
                            // Calibrate context window with actual API usage immediately
                            // (must happen here, not after the loop, because early returns bypass post-loop code)
                            self.context_window.update_usage_from_response(usage);

                            // Update cumulative cache statistics
                            self.cache_stats.total_calls += 1;
                            self.cache_stats.total_input_tokens += usage.prompt_tokens as u64;
                            self.cache_stats.total_cache_creation_tokens +=
                                usage.cache_creation_tokens as u64;
                            self.cache_stats.total_cache_read_tokens +=
                                usage.cache_read_tokens as u64;
                            if usage.cache_read_tokens > 0 {
                                self.cache_stats.cache_hit_calls += 1;
                            }
                            debug!(
                                "Received usage data - prompt: {}, completion: {}, total: {}",
                                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                            );
                        }

                        // Handle tool call streaming hint (show UI indicator immediately)
                        if let Some(ref tool_name) = chunk.tool_call_streaming {
                            if tool_name.is_empty() {
                                // Empty string = "active" hint for blinking
                                self.ui_writer.print_tool_streaming_active();
                            } else {
                                // Non-empty = "detected" hint with tool name
                                self.ui_writer.print_tool_streaming_hint(tool_name);
                            }
                        }

                        // Store raw chunk for debugging (limit to first 20 and last 5)
                        if iter.chunks_received < 20 || chunk.finished {
                            iter.raw_chunks.push(format!(
                                "Chunk #{}: content={:?}, finished={}, tool_calls={:?}",
                                iter.chunks_received + 1,
                                chunk.content,
                                chunk.finished,
                                chunk.tool_calls
                            ));
                        } else if iter.raw_chunks.len() == 20 {
                            iter.raw_chunks.push("... (chunks 21+ omitted for brevity) ...".to_string());
                        }

                        // Record time to first token
                        if state.first_token_time.is_none() && !chunk.content.is_empty() {
                            state.first_token_time = Some(state.stream_start.elapsed());
                            // Record in agent metrics
                            if let Some(ttft) = state.first_token_time {
                                self.first_token_times.push(ttft);
                            }
                        }

                        iter.chunks_received += 1;
                        if iter.chunks_received == 1 {
                            debug!(
                                "First chunk received: content_len={}, finished={}",
                                chunk.content.len(),
                                chunk.finished
                            );
                        }

                        // Process chunk with the new parser
                        let completed_tools = iter.parser.process_chunk(&chunk);

                        // Handle completed tool calls - process all if multiple calls enabled
                        // Always process all tool calls - they will be executed after stream ends

                        // De-duplicate tool calls (sequential duplicates in chunk + duplicates from previous message)
                        let last_executed_in_iter = iter.last_executed_tool.clone();
                        let deduplicated_tools =
                            streaming::deduplicate_tool_calls(completed_tools, |tc| {
                                // First check against the last tool executed in this
                                // iteration (catches duplicates across chunks within
                                // the same streaming response).
                                if let Some(ref last) = last_executed_in_iter {
                                    if streaming::are_tool_calls_duplicate(last, tc) {
                                        return Some("DUP IN ITER".to_string());
                                    }
                                }
                                // Then check against messages from before this iteration
                                self.check_duplicate_in_previous_message(tc, history_len_before_iteration)
                            });

                        // Process each tool call
                        for (tool_call, duplicate_type) in deduplicated_tools {
                            debug!("Processing completed tool call: {:?}", tool_call);

                            // Skip duplicates (don't set tool_executed - would trigger spurious auto-continue)
                            if let Some(dup_type) = &duplicate_type {
                                debug!(
                                    "Skipping duplicate tool call ({}): {} with args {}",
                                    dup_type,
                                    tool_call.tool,
                                    serde_json::to_string(&tool_call.args)
                                        .unwrap_or_else(|_| "<unserializable>".to_string())
                                );
                                continue;
                            }

                            // Flag for post-turn compaction if at 90% capacity
                            if self.auto_compact && self.context_window.percentage_used() >= 90.0 {
                                self.pending_90_compaction = true;
                            }

                            // Thin context if needed before tool execution
                            if self.context_window.should_thin() {
                                let thin_result = self.do_thin_context();
                                self.ui_writer.print_thin_result(&thin_result);
                            }

                            // Calculate new content to display (skip already-shown text)
                            let already_displayed_chars = iter.current_response.chars().count();
                            let text_content = iter.parser.get_text_content();
                            let clean_content = streaming::clean_llm_tokens(&text_content);
                            // Use only the text before tool calls for the log message.
                            // This prevents duplicate tool call JSON from being stored
                            // in the assistant message when the LLM stutters.
                            let raw_content_for_log = streaming::clean_llm_tokens(iter.parser.get_text_before_tool_calls());
                            let filtered_content =
                                self.ui_writer.filter_json_tool_calls(&clean_content);
                            let final_display_content = filtered_content.trim();

                            // Extract only the new (undisplayed) portion
                            let new_content =
                                if iter.current_response.len() <= final_display_content.len() {
                                    final_display_content
                                        .chars()
                                        .skip(already_displayed_chars)
                                        .collect::<String>()
                                } else {
                                    String::new()
                                };

                            // Display new text before tool execution
                            if !new_content.trim().is_empty() {
                                #[allow(unused_assignments)]
                                if !state.response_started {
                                    self.ui_writer.print_agent_prompt();
                                    state.response_started = true;
                                }
                                self.ui_writer.print_agent_response(&new_content);
                                self.ui_writer.flush();
                                iter.current_response.push_str(&new_content);
                            }

                            self.ui_writer.finish_streaming_markdown();

                            let is_todo_tool = streaming::is_self_handled_tool(&tool_call.tool);
                            let is_compact_tool = streaming::is_compact_tool(&tool_call.tool);

                            // Tool call header (TODO tools print their own)
                            if !is_todo_tool {
                                self.ui_writer
                                    .print_tool_header(&tool_call.tool, Some(&tool_call.args));
                                if let Some(args_obj) = tool_call.args.as_object() {
                                    for (key, value) in args_obj {
                                        let value_str = streaming::format_tool_arg_value(
                                            &tool_call.tool,
                                            key,
                                            value,
                                        );
                                        self.ui_writer.print_tool_arg(key, &value_str);
                                    }
                                }
                            }

                            // Only print output header for non-compact tools
                            if !is_compact_tool && !is_todo_tool {
                                self.ui_writer.print_tool_output_header();
                            }

                            // Clone working_dir to avoid borrow checker issues
                            let working_dir = self.working_dir.clone();
                            let exec_start = Instant::now();
                            // Tool execution timeout: 20 minutes for research, 8 minutes for others
                            let timeout_duration = if tool_call.tool == "research" {
                                Duration::from_secs(20 * 60) // 20 minutes for research
                            } else {
                                Duration::from_secs(8 * 60) // 8 minutes for other tools
                            };
                            let tool_result = match tokio::time::timeout(
                                timeout_duration,
                                // Use working_dir if set (from --codebase-fast-start)
                                self.execute_tool_in_dir(&tool_call, working_dir.as_deref()),
                            )
                            .await
                            {
                                Ok(result) => result?,
                                Err(_) => {
                                    let timeout_mins = if tool_call.tool == "research" { 20 } else { 8 };
                                    warn!("Tool call {} timed out after {} minutes", tool_call.tool, timeout_mins);
                                    format!(
                                        "❌ Tool execution timed out after {} minutes",
                                        timeout_mins
                                    )
                                }
                            };
                            let exec_duration = exec_start.elapsed();

                            // Track tool call metrics
                            let tool_success = !tool_result.contains("❌");
                            self.tool_call_metrics.push((
                                tool_call.tool.clone(),
                                exec_duration,
                                tool_success,
                            ));

                            // Display tool execution result with proper indentation
                            let compact_summary = {
                                let output_lines: Vec<&str> = tool_result.lines().collect();
                                let wants_full = self.ui_writer.wants_full_output();
                                const MAX_LINES: usize = 5;
                                const MAX_LINE_WIDTH: usize = 80;
                                let output_len = output_lines.len();

                                // Use centralized tool output formatting
                                match streaming::format_tool_result_summary(
                                    &tool_call.tool,
                                    &tool_result,
                                    tool_success,
                                ) {
                                    streaming::ToolOutputFormat::SelfHandled => None,
                                    streaming::ToolOutputFormat::Compact(summary) => Some(summary),
                                    streaming::ToolOutputFormat::Regular => {
                                    // Regular tools: show truncated output lines
                                    let max_lines_to_show =
                                        if wants_full { output_len } else { MAX_LINES };
                                    for (idx, line) in output_lines.iter().enumerate() {
                                        if !wants_full && idx >= max_lines_to_show {
                                            break;
                                        }
                                        self.ui_writer.update_tool_output_line(
                                            &streaming::truncate_line(
                                                line,
                                                MAX_LINE_WIDTH,
                                                !wants_full,
                                            ),
                                        );
                                    }
                                    if !wants_full && output_len > MAX_LINES {
                                        self.ui_writer.print_tool_output_summary(output_len);
                                    }
                                    None
                                    }
                                }
                            };

                            // Add the tool call and result to the context window using RAW unfiltered content
                            // This ensures the log file contains the true raw content including JSON tool calls
                            let tool_message = {
                                let text_content = raw_content_for_log.trim().to_string();
                                let mut msg = Message::new(
                                    MessageRole::Assistant,
                                    text_content,
                                );
                                // Store the tool call structurally so that providers can
                                // emit proper tool_use blocks (e.g. Anthropic API) instead
                                // of inline JSON text that confuses the model.
                                msg.tool_calls.push(g3_providers::MessageToolCall {
                                    id: if tool_call.id.is_empty() {
                                        // Safety net: generate an ID if none was provided.
                                        // Anthropic API requires tool_use IDs matching ^[a-zA-Z0-9_-]+$
                                        use std::sync::atomic::{AtomicU64, Ordering};
                                        static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
                                        format!("tool_{}", FALLBACK_COUNTER.fetch_add(1, Ordering::SeqCst))
                                    } else {
                                        tool_call.id.clone()
                                    },
                                    name: tool_call.tool.clone(),
                                    input: tool_call.args.clone(),
                                });
                                msg
                            };
                            let mut result_message = {
                                let content = format!("Tool result: {}", tool_result);

                                // Apply cache control every 10 tool calls (max 4 annotations)
                                let should_cache = self.tool_call_count > 0
                                    && self.tool_call_count % 10 == 0
                                    && self.count_cache_controls_in_history() < 4;

                                if should_cache {
                                    let provider = self.providers.get(None)?;
                                    if let Some(cache_config) = self.get_provider_cache_control() {
                                        Message::with_cache_control_validated(
                                            MessageRole::User,
                                            content,
                                            cache_config,
                                            provider,
                                        )
                                    } else {
                                        Message::new(MessageRole::User, content)
                                    }
                                } else {
                                    Message::new(MessageRole::User, content)
                                }
                            };

                            // Link the tool result to the tool_use ID so providers can
                            // emit proper tool_result blocks (e.g. Anthropic API).
                            result_message.tool_result_id = Some(tool_call.id.clone());

                            // Attach any pending images to the result message
                            // (images loaded via read_image tool)
                            if !self.pending_images.is_empty() {
                                result_message.images = std::mem::take(&mut self.pending_images);
                            }

                            // Track tokens before adding messages
                            let tokens_before = self.context_window.used_tokens;

                            self.context_window.add_message(tool_message);
                            self.context_window.add_message(result_message);

                            // Closure marker with timing
                            let tokens_delta = self
                                .context_window
                                .used_tokens
                                .saturating_sub(tokens_before);

                            // TODO tools handle their own output via print_todo_compact, skip timing
                            if !is_todo_tool {
                                // Use compact format for file operations, normal format for others
                                if let Some(summary) = compact_summary {
                                    self.ui_writer.print_tool_compact(
                                        &tool_call.tool,
                                        &summary,
                                        &streaming::format_duration(exec_duration),
                                        tokens_delta,
                                        self.context_window.percentage_used(),
                                    );
                                } else {
                                    self.ui_writer.print_tool_timing(
                                        &streaming::format_duration(exec_duration),
                                        tokens_delta,
                                        self.context_window.percentage_used(),
                                    );
                                }
                            }
                            self.ui_writer.print_agent_prompt();

                            // Update the request with the new context for next iteration
                            request.messages = self.context_window.conversation_history.clone();

                            // Ensure tools are included for native providers in subsequent iterations
                            let provider_for_tools = self.providers.get(None)?;
                            if provider_for_tools.has_native_tool_calling() {
                                let tool_config = tool_definitions::ToolConfig::new(
                                    self.config.computer_control.enabled,
                                );
                                request.tools =
                                    Some(self.get_tool_definitions_with_loaded_toolsets(tool_config));
                            }

                            // DO NOT add final_display_content to full_response here!
                            // The content was already displayed during streaming and added to current_response.
                            // Adding it again would cause duplication when the agent message is printed.
                            // The only time we should add to full_response is:
                            // 1. At the end when no tools were executed
                            // 2. At the end when no tools were executed (handled in the "no tool executed" branch)

                            iter.tool_executed = true;
                            iter.last_executed_tool = Some(tool_call.clone());
                            state.any_tool_executed = true; // Track across all iterations

                            // Reset the JSON tool call filter state after each tool execution
                            // This ensures the filter doesn't stay in suppression mode for subsequent streaming content
                            self.ui_writer.reset_json_filter();

                            // Only reset parser if there are no more unexecuted tool calls in the buffer
                            // This handles the case where the LLM emits multiple tool calls in one response
                            if iter.parser.has_unexecuted_tool_call() {
                                debug!(
                                    "Parser still has unexecuted tool calls, not resetting buffer"
                                );
                                // Mark current tool as consumed so we don't re-detect it
                                iter.parser.mark_tool_calls_consumed();
                            } else {
                                // Reset parser for next iteration - this clears the text buffer
                                iter.parser.reset();
                            }

                            // Clear current_response for next iteration to prevent buffered text
                            // from being incorrectly displayed after tool execution
                            iter.current_response.clear();
                            // Reset for next iteration (value read in next loop pass)
                            state.response_started = false;

                            // Continue processing - don't break mid-stream
                        } // End of for loop processing each tool call

                        // Note: We no longer break mid-stream after tool execution.
                        // All tool calls are collected and executed after the stream ends.

                        // If no tool calls were completed, continue streaming normally
                        if !iter.tool_executed {
                            let clean_content = streaming::clean_llm_tokens(&chunk.content);

                            if !clean_content.is_empty() {
                                let filtered_content =
                                    self.ui_writer.filter_json_tool_calls(&clean_content);

                                if !filtered_content.is_empty() {
                                    if !state.response_started {
                                        self.ui_writer.print_agent_prompt();
                                        state.response_started = true;
                                    }

                                    self.ui_writer.print_agent_response(&filtered_content);
                                    self.ui_writer.flush();
                                    iter.current_response.push_str(&filtered_content);
                                }
                            }
                        }

                        if chunk.finished {
                            debug!("Stream finished: tool_executed={}, current_response_len={}, full_response_len={}, chunks_received={}",
                                iter.tool_executed, iter.current_response.len(), state.full_response.len(), iter.chunks_received);

                            // Capture the stop reason from the final chunk
                            if let Some(ref reason) = chunk.stop_reason {
                                debug!("Stream stop_reason: {}", reason);
                                iter.stream_stop_reason = Some(reason.clone());
                            }

                            // Stream finished - check if we should continue or return
                            if !iter.tool_executed {
                                // No tools were executed in this iteration
                                // Check if we got any meaningful response at all
                                // We need to check the parser's text buffer as well, since the LLM
                                // might have responded with text but no tool calls
                                let text_content = iter.parser.get_text_content();
                                let has_text_response = !text_content.trim().is_empty()
                                    || !iter.current_response.trim().is_empty();

                                // Don't re-add text from parser buffer if we already displayed it
                                // The parser buffer contains ALL accumulated text, but current_response
                                // already has what was displayed during streaming
                                if iter.current_response.is_empty() && !text_content.trim().is_empty() {
                                    // Only use parser text if we truly have no response
                                    // This should be rare - only if streaming failed to display anything
                                    debug!("Warning: Using parser buffer text as fallback - this may duplicate output");
                                    // Extract only the undisplayed portion from parser buffer
                                    // Parser buffer accumulates across iterations, so we need to be careful
                                    let clean_text = streaming::clean_llm_tokens(&text_content);

                                    let filtered_text =
                                        self.ui_writer.filter_json_tool_calls(&clean_text);

                                    // Only use this if we truly have nothing else
                                    if !filtered_text.trim().is_empty() && state.full_response.is_empty()
                                    {
                                        debug!(
                                            "Using filtered parser text as last resort: {} chars",
                                            filtered_text.len()
                                        );
                                        // Note: This assignment is currently unused but kept for potential future use
                                        let _ = filtered_text;
                                    }
                                }

                                if !has_text_response && state.full_response.is_empty() {
                                    streaming::log_stream_error(
                                        state.iteration_count,
                                        &provider_name,
                                        &provider_model,
                                        iter.chunks_received,
                                        &iter.parser,
                                        &request,
                                        &self.context_window,
                                        self.session_id.as_deref(),
                                        &iter.raw_chunks,
                                    );

                                    // No response received - this is an error condition
                                    warn!("Stream finished without any content or tool calls");
                                    warn!("Chunks received: {}", iter.chunks_received);
                                    return Err(anyhow::anyhow!(
                                        "No response received from the model. The model may be experiencing issues or the request may have been malformed."
                                    ));
                                }

                                // If tools were executed in previous iterations,
                                // break to let the outer loop handle finalization
                                if state.any_tool_executed {
                                    debug!("Tools were executed in previous iterations, breaking to finalize");
                                    // IMPORTANT: Save any text response to context window before breaking
                                    // This ensures text displayed after tool execution is not lost
                                    if !iter.current_response.trim().is_empty() && !state.assistant_message_added {
                                        debug!("Saving current_response ({} chars) to context before finalization", iter.current_response.len());
                                        let assistant_msg = Message::new(
                                            MessageRole::Assistant,
                                            iter.current_response.clone(),
                                        );
                                        self.context_window.add_message(assistant_msg);
                                        state.assistant_message_added = true;
                                    }

                                    // NOTE: We intentionally do NOT set full_response here.
                                    // The content was already displayed during streaming.
                                    // Setting full_response would cause duplication when the
                                    // function eventually returns.
                                    // Context window is updated separately via add_message().
                                    break;
                                }

                                // Save assistant message before returning (no tools were executed)
                                // This ensures text-only responses are saved to context
                                if !iter.current_response.trim().is_empty() && !state.assistant_message_added {
                                    debug!("Saving current_response ({} chars) to context before early return", iter.current_response.len());
                                    let assistant_msg = Message::new(
                                        MessageRole::Assistant,
                                        iter.current_response.clone(),
                                    );
                                    self.context_window.add_message(assistant_msg);
                                    // state.assistant_message_added = true; // Not needed, we're returning
                                }

                                // Set full_response to empty to avoid duplication in return value
                                // (content was already displayed during streaming)
                                return Ok(self.finalize_streaming_turn(
                                    String::new(),
                                    show_timing,
                                    state.stream_start,
                                    state.first_token_time,
                                    &state.turn_accumulated_usage,
                                ));
                            }
                            break; // Tool was executed, break to continue outer loop
                        }
                    }
                    Err(e) => {
                        // Capture detailed streaming error information
                        let error_msg = e.to_string();
                        let error_details = format!(
                            "Streaming error at chunk {}: {}",
                            iter.chunks_received + 1,
                            error_msg
                        );

                        error!("Error type: {}", std::any::type_name_of_val(&e));
                        error!("Parser state at error: text_buffer_len={}, has_incomplete={}, message_stopped={}",
                            iter.parser.text_buffer_len(), iter.parser.has_incomplete_tool_call(), iter.parser.is_message_stopped());

                        // Check if this is a recoverable connection error
                        let is_connection_error = streaming::is_connection_error(&error_msg);

                        if is_connection_error {
                            warn!(
                                "Connection error at chunk {}, treating as end of stream",
                                iter.chunks_received + 1
                            );
                            // If we have any content or tool calls, treat this as a graceful end
                            if iter.chunks_received > 0
                                && (!iter.parser.get_text_content().is_empty()
                                    || iter.parser.has_unexecuted_tool_call())
                            {
                                warn!("Stream terminated unexpectedly but we have content, continuing");
                                break; // Break to process what we have
                            }
                        }

                        if iter.tool_executed {
                            error!("{}", error_details);
                            warn!("Stream error after tool execution, attempting to continue");
                            break; // Break to outer loop to start new stream
                        } else {
                            // Log raw chunks before failing
                            error!("Fatal streaming error. Raw chunks received before error:");
                            for chunk_str in iter.raw_chunks.iter().take(10) {
                                error!("  {}", chunk_str);
                            }
                            return Err(e);
                        }
                    }
                }
            }

            // Fall back to estimation if no usage data was provided by the stream
            // (calibration already happened inline when usage data arrived)
            if iter.accumulated_usage.is_none() {
                debug!("No usage data from stream, using estimation");
                let estimated_tokens = ContextWindow::estimate_tokens(&iter.current_response);
                self.context_window.add_streaming_tokens(estimated_tokens);
            }

            // If we get here and no tool was executed, we're done
            if !iter.tool_executed {
                // IMPORTANT: Do NOT add parser text_content here!
                // The text has already been displayed during streaming via current_response.
                // The parser buffer accumulates ALL text and would cause duplication.
                debug!("Stream completed without tool execution. Response already displayed during streaming.");
                debug!(
                    "Current response length: {}, Full response length: {}",
                    iter.current_response.len(),
                    state.full_response.len()
                );

                let has_response = !iter.current_response.is_empty() || !state.full_response.is_empty();

                // Check if the response is essentially empty (just whitespace or timing lines)
                // Check if there's an incomplete tool call in the buffer (for debugging)
                let has_incomplete_tool_call = iter.parser.has_incomplete_tool_call();

                // Check if there's a complete but unexecuted tool call in the buffer (for debugging)
                let has_unexecuted_tool_call = iter.parser.has_unexecuted_tool_call();

                // Log when we detect unexecuted or incomplete tool calls for debugging
                if has_incomplete_tool_call {
                    debug!("Detected incomplete tool call in buffer (buffer_len={}, consumed_up_to={})",
                        iter.parser.text_buffer_len(), iter.parser.text_buffer_len());
                }
                if has_unexecuted_tool_call {
                    debug!("Detected unexecuted tool call in buffer - this may indicate a parsing issue");
                    warn!("Unexecuted tool call detected in buffer after stream ended");
                }

                // Check if the response was truncated due to max_tokens
                let was_truncated_by_max_tokens =
                    iter.stream_stop_reason.as_deref() == Some("max_tokens");
                if was_truncated_by_max_tokens {
                    debug!("Response was truncated due to max_tokens limit");
                    warn!("LLM response was cut off due to max_tokens limit");
                }

                // Log response status for debugging
                if has_response {
                    debug!(
                        "Response already streamed, not setting full_response. current_response: {} chars",
                        iter.current_response.len()
                    );
                }

                // Add the RAW unfiltered response to context window before returning.
                // This ensures the log contains the true raw content including any JSON.
                // Note: We check current_response, not full_response, because full_response
                // may be empty to avoid display duplication (content was already streamed).
                if !iter.current_response.trim().is_empty() && !state.assistant_message_added {
                    // Get the raw text from the parser (before filtering)
                    let raw_text = iter.parser.get_text_content();
                    let raw_clean = streaming::clean_llm_tokens(&raw_text);

                    // Use raw_clean if available, otherwise fall back to current_response.
                    // This fixes a bug where the parser buffer might be empty/cleared
                    // even though current_response has content that was displayed.
                    let content_to_save = if !raw_clean.trim().is_empty() {
                        raw_clean
                    } else {
                        iter.current_response.clone()
                    };
                    let assistant_message = Message::new(MessageRole::Assistant, content_to_save);
                    self.context_window.add_message(assistant_message);
                }

                return Ok(self.finalize_streaming_turn(
                    state.full_response.clone(),
                    show_timing,
                    state.stream_start,
                    state.first_token_time,
                    &state.turn_accumulated_usage,
                ));
            }

            // Continue the loop to start a new stream with updated context
        }

        // --- Phase 4: Post-Loop Finalization ---
        Ok(self.finalize_streaming_turn(
            state.full_response.clone(),
            show_timing,
            state.stream_start,
            state.first_token_time,
            &state.turn_accumulated_usage,
        ))
    }

    // =========================================================================
    // TOOL EXECUTION
    // =========================================================================

    pub async fn execute_tool(&mut self, tool_call: &ToolCall) -> Result<String> {
        // Tool tracking is handled by execute_tool_in_dir
        self.execute_tool_in_dir(tool_call, None).await
    }

    /// Execute a tool with an optional working directory (for discovery commands)
    pub async fn execute_tool_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        // Always track tool calls for auto-memory feature
        self.tool_call_count += 1;
        self.tool_calls_this_turn.push(tool_call.tool.clone());

        let result = self.execute_tool_inner_in_dir(tool_call, working_dir).await;

        // Check plan approval gate after tool execution (only in plan mode)
        if self.in_plan_mode {
            if let Some(session_id) = &self.session_id {
            if let ApprovalGateResult::Blocked { message } =
                check_plan_approval_gate(session_id, working_dir, &self.baseline_dirty_files)
            {
                // Return the blocking message instead of the tool result
                return Ok(message);
            }
            }
        }

        let log_str = match &result {
            Ok(s) => s.clone(),
            Err(e) => format!("ERROR: {}", e),
        };
        debug!(
            "Tool {} completed: {}",
            tool_call.tool,
            &log_str.chars().take(100).collect::<String>()
        );
        result
    }

    async fn execute_tool_inner_in_dir(
        &mut self,
        tool_call: &ToolCall,
        working_dir: Option<&str>,
    ) -> Result<String> {
        debug!("=== EXECUTING TOOL ===");
        debug!("Tool name: {}", tool_call.tool);
        debug!(
            "Working directory passed to execute_tool_inner_in_dir: {:?}",
            working_dir
        );
        debug!("Tool args (raw): {:?}", tool_call.args);
        debug!(
            "Tool args (JSON): {}",
            serde_json::to_string(&tool_call.args)
                .unwrap_or_else(|_| "failed to serialize".to_string())
        );
        debug!("======================");

        // Create tool context for dispatch
        let mut ctx = tools::executor::ToolContext {
            config: &self.config,
            ui_writer: &self.ui_writer,
            session_id: self.session_id.as_deref(),
            working_dir,
            computer_controller: self.computer_controller.as_ref(),
            webdriver_session: &self.webdriver_session,
            webdriver_process: &self.webdriver_process,
            background_process_manager: &self.background_process_manager,
            todo_content: &self.todo_content,
            pending_research_manager: &self.pending_research_manager,
            pending_images: &mut self.pending_images,
            is_autonomous: self.is_autonomous,
            requirements_sha: self.requirements_sha.as_deref(),
            context_total_tokens: self.context_window.total_tokens,
            context_used_tokens: self.context_window.used_tokens,
            loaded_toolsets: &mut self.loaded_toolsets,
        };

        // Dispatch to the appropriate tool handler
        let result = tool_dispatch::dispatch_tool(tool_call, &mut ctx).await?;

        Ok(result)
    }
}

// Re-export utility functions
pub use utils::apply_unified_diff_to_string;
use utils::truncate_to_word_boundary;

/// First line of a message, ellipsised, for one-line status output.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    // Char-based, not byte-based: slicing a multi-byte char would panic.
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

// Implement Drop to clean up safaridriver process
impl<W: UiWriter> Drop for Agent<W> {
    fn drop(&mut self) {
        // Validate system prompt invariant on drop (agent exit)
        // This catches any bugs where the conversation history was corrupted during execution
        if !self.context_window.conversation_history.is_empty() {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.validate_system_prompt_is_first();
            })) {
                eprintln!(
                    "\n⚠️  FATAL ERROR ON EXIT: System prompt validation failed: {:?}",
                    e
                );
            }
        }

        // Try to kill safaridriver process if it's still running
        // We need to use try_lock since we can't await in Drop
        if let Ok(mut process_guard) = self.webdriver_process.try_write() {
            if let Some(process) = process_guard.take() {
                // Use blocking kill since we can't await in Drop
                // This is a best-effort cleanup
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(process.id().unwrap_or(0).to_string())
                    .output();

                debug!("Attempted to clean up safaridriver process on Agent drop");
            }
        }
    }
}

#[cfg(test)]
mod tool_timeout_tests {
    use std::time::Duration;

    /// Get the timeout duration for a tool (extracted for testing)
    fn get_tool_timeout(tool_name: &str) -> Duration {
        if tool_name == "research" {
            Duration::from_secs(20 * 60) // 20 minutes for research
        } else {
            Duration::from_secs(8 * 60) // 8 minutes for other tools
        }
    }

    #[test]
    fn test_research_tool_has_20_minute_timeout() {
        let timeout = get_tool_timeout("research");
        assert_eq!(timeout, Duration::from_secs(20 * 60));
        assert_eq!(timeout.as_secs(), 1200); // 20 minutes in seconds
    }

    #[test]
    fn test_shell_tool_has_8_minute_timeout() {
        let timeout = get_tool_timeout("shell");
        assert_eq!(timeout, Duration::from_secs(8 * 60));
        assert_eq!(timeout.as_secs(), 480); // 8 minutes in seconds
    }

    #[test]
    fn test_read_file_tool_has_8_minute_timeout() {
        let timeout = get_tool_timeout("read_file");
        assert_eq!(timeout, Duration::from_secs(8 * 60));
    }

    #[test]
    fn test_write_file_tool_has_8_minute_timeout() {
        let timeout = get_tool_timeout("write_file");
        assert_eq!(timeout, Duration::from_secs(8 * 60));
    }

    #[test]
    fn test_str_replace_tool_has_8_minute_timeout() {
        let timeout = get_tool_timeout("str_replace");
        assert_eq!(timeout, Duration::from_secs(8 * 60));
    }

    #[test]
    fn test_code_search_tool_has_8_minute_timeout() {
        let timeout = get_tool_timeout("code_search");
        assert_eq!(timeout, Duration::from_secs(8 * 60));
    }

    #[test]
    fn test_webdriver_tools_have_8_minute_timeout() {
        for tool in ["webdriver_start", "webdriver_navigate", "webdriver_click"] {
            let timeout = get_tool_timeout(tool);
            assert_eq!(timeout, Duration::from_secs(8 * 60), "Tool {} should have 8 min timeout", tool);
        }
    }

    #[test]
    fn test_only_research_has_extended_timeout() {
        // List of all tools that should have 8-minute timeout
        let standard_tools = [
            "shell", "read_file", "write_file", "str_replace", "read_image",
            "screenshot", "code_search", "todo_read", "todo_write", "remember",
            "rehydrate", "coverage", "background_process",
            "webdriver_start", "webdriver_navigate", "webdriver_click",
            "webdriver_send_keys", "webdriver_find_element", "webdriver_quit",
        ];

        for tool in standard_tools {
            let timeout = get_tool_timeout(tool);
            assert_eq!(
                timeout,
                Duration::from_secs(8 * 60),
                "Tool '{}' should have standard 8-minute timeout, not extended",
                tool
            );
        }

        // Only research should have extended timeout
        let research_timeout = get_tool_timeout("research");
        assert_eq!(
            research_timeout,
            Duration::from_secs(20 * 60),
            "Research tool should have 20-minute timeout"
        );
    }
}
