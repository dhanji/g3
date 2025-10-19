pub mod error_handling;
pub mod project;
pub mod task_result;
pub mod ui_writer;
pub use task_result::TaskResult;

#[cfg(test)]
mod task_result_comprehensive_tests;
use crate::ui_writer::UiWriter;

mod fixed_filter_json;
#[cfg(test)]
mod fixed_filter_tests;

#[cfg(test)]
mod tilde_expansion_tests;

#[cfg(test)]
mod error_handling_test;
use anyhow::Result;
use g3_config::Config;
use g3_execution::CodeExecutor;
use g3_providers::{CompletionRequest, Message, MessageRole, ProviderRegistry, Tool};
#[allow(unused_imports)]
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value, // Should be a JSON object with tool-specific arguments
}

#[derive(Debug, Clone)]
pub enum StreamState {
    Generating,
    ToolDetected(ToolCall),
    Executing,
    Resuming,
}

/// Modern streaming tool parser that properly handles native tool calls and SSE chunks
#[derive(Debug)]
pub struct StreamingToolParser {
    /// Buffer for accumulating text content
    text_buffer: String,
    /// Buffer for accumulating native tool calls
    native_tool_calls: Vec<g3_providers::ToolCall>,
    /// Whether we've received a message_stop event
    message_stopped: bool,
    /// Whether we're currently in a JSON tool call (for fallback parsing)
    in_json_tool_call: bool,
    /// Start position of JSON tool call (for fallback parsing)
    json_tool_start: Option<usize>,
}

impl StreamingToolParser {
    pub fn new() -> Self {
        Self {
            text_buffer: String::new(),
            native_tool_calls: Vec::new(),
            message_stopped: false,
            in_json_tool_call: false,
            json_tool_start: None,
        }
    }

    /// Process a streaming chunk and return completed tool calls if any
    pub fn process_chunk(&mut self, chunk: &g3_providers::CompletionChunk) -> Vec<ToolCall> {
        let mut completed_tools = Vec::new();

        // Add text content to buffer
        if !chunk.content.is_empty() {
            self.text_buffer.push_str(&chunk.content);
        }

        // Handle native tool calls
        if let Some(ref tool_calls) = chunk.tool_calls {
            debug!("Received native tool calls: {:?}", tool_calls);

            // Accumulate native tool calls
            for tool_call in tool_calls {
                self.native_tool_calls.push(tool_call.clone());
            }
        }

        // Check if message is finished/stopped
        if chunk.finished {
            self.message_stopped = true;
            debug!("Message finished, processing accumulated tool calls");
        }

        // If we have native tool calls and the message is stopped, return them
        if self.message_stopped && !self.native_tool_calls.is_empty() {
            debug!(
                "Converting {} native tool calls",
                self.native_tool_calls.len()
            );

            for native_tool in &self.native_tool_calls {
                let converted_tool = ToolCall {
                    tool: native_tool.tool.clone(),
                    args: native_tool.args.clone(),
                };
                completed_tools.push(converted_tool);
            }

            // Clear native tool calls after processing
            self.native_tool_calls.clear();
        }

        // Fallback: Try to parse JSON tool calls from text if no native tool calls
        if completed_tools.is_empty() && !chunk.content.is_empty() {
            if let Some(json_tool) = self.try_parse_json_tool_call(&chunk.content) {
                completed_tools.push(json_tool);
            }
        }

        completed_tools
    }

    /// Fallback method to parse JSON tool calls from text content
    fn try_parse_json_tool_call(&mut self, _content: &str) -> Option<ToolCall> {
        // Look for JSON tool call patterns
        let patterns = [
            r#"{"tool":"#,
            r#"{ "tool":"#,
            r#"{"tool" :"#,
            r#"{ "tool" :"#,
        ];

        // If we're not currently in a JSON tool call, look for the start
        if !self.in_json_tool_call {
            for pattern in &patterns {
                if let Some(pos) = self.text_buffer.rfind(pattern) {
                    debug!(
                        "Found JSON tool call pattern '{}' at position {}",
                        pattern, pos
                    );
                    self.in_json_tool_call = true;
                    self.json_tool_start = Some(pos);
                    break;
                }
            }
        }

        // If we're in a JSON tool call, try to find the end and parse it
        if self.in_json_tool_call {
            if let Some(start_pos) = self.json_tool_start {
                let json_text = &self.text_buffer[start_pos..];

                // Try to find a complete JSON object
                let mut brace_count = 0;
                let mut in_string = false;
                let mut escape_next = false;

                for (i, ch) in json_text.char_indices() {
                    if escape_next {
                        escape_next = false;
                        continue;
                    }

                    match ch {
                        '\\' => escape_next = true,
                        '"' if !escape_next => in_string = !in_string,
                        '{' if !in_string => brace_count += 1,
                        '}' if !in_string => {
                            brace_count -= 1;
                            if brace_count == 0 {
                                // Found complete JSON object
                                let json_str = &json_text[..=i];
                                debug!("Attempting to parse JSON tool call: {}", json_str);

                                // First try to parse as a ToolCall
                                if let Ok(tool_call) = serde_json::from_str::<ToolCall>(json_str) {
                                    // Validate that this is actually a proper tool call
                                    // The args should be a JSON object with reasonable keys
                                    if let Some(args_obj) = tool_call.args.as_object() {
                                        // Check if any key looks like it contains agent message content
                                        // This would indicate a malformed tool call where the message
                                        // got mixed into the args
                                        let has_message_like_key = args_obj.keys().any(|key| {
                                            key.len() > 100
                                                || key.contains('\n')
                                                || key.contains("I'll")
                                                || key.contains("Let me")
                                                || key.contains("Here's")
                                                || key.contains("I can")
                                                || key.contains("I need")
                                                || key.contains("First")
                                                || key.contains("Now")
                                                || key.contains("The ")
                                        });

                                        if has_message_like_key {
                                            debug!("Detected malformed tool call with message-like keys, skipping");
                                            // This looks like a malformed tool call, skip it
                                            self.in_json_tool_call = false;
                                            self.json_tool_start = None;
                                            break;
                                        }

                                        // Also check if the values look reasonable
                                        // Tool arguments should typically be file paths, commands, or content
                                        // Not entire agent messages

                                        debug!(
                                            "Successfully parsed valid JSON tool call: {:?}",
                                            tool_call
                                        );
                                        // Reset JSON parsing state
                                        self.in_json_tool_call = false;
                                        self.json_tool_start = None;
                                        return Some(tool_call);
                                    }
                                    // If args is not an object, skip this as invalid
                                    debug!("Tool call args is not an object, skipping");
                                } else {
                                    debug!("Failed to parse JSON tool call: {}", json_str);
                                    // Reset and continue looking
                                    self.in_json_tool_call = false;
                                    self.json_tool_start = None;
                                }
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        None
    }

    /// Get the accumulated text content (excluding tool calls)
    pub fn get_text_content(&self) -> &str {
        &self.text_buffer
    }

    /// Get content before a specific position (for display purposes)
    pub fn get_content_before_position(&self, pos: usize) -> String {
        if pos <= self.text_buffer.len() {
            self.text_buffer[..pos].to_string()
        } else {
            self.text_buffer.clone()
        }
    }

    /// Check if the message has been stopped/finished
    pub fn is_message_stopped(&self) -> bool {
        self.message_stopped
    }

    /// Reset the parser state for a new message
    pub fn reset(&mut self) {
        self.text_buffer.clear();
        self.native_tool_calls.clear();
        self.message_stopped = false;
        self.in_json_tool_call = false;
        self.json_tool_start = None;
    }

    /// Get the current text buffer length (for position tracking)
    pub fn text_buffer_len(&self) -> usize {
        self.text_buffer.len()
    }
}

#[derive(Debug, Clone)]
pub struct ContextWindow {
    pub used_tokens: u32,
    pub total_tokens: u32,
    pub cumulative_tokens: u32, // Track cumulative tokens across all interactions
    pub conversation_history: Vec<Message>,
}

impl ContextWindow {
    pub fn new(total_tokens: u32) -> Self {
        Self {
            used_tokens: 0,
            total_tokens,
            cumulative_tokens: 0,
            conversation_history: Vec::new(),
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.add_message_with_tokens(message, None);
    }

    /// Add a message with optional token count from the provider
    pub fn add_message_with_tokens(&mut self, message: Message, tokens: Option<u32>) {
        // Skip messages with empty content to avoid API errors
        if message.content.trim().is_empty() {
            warn!("Skipping empty message to avoid API error");
            return;
        }

        // Use provided token count if available, otherwise estimate
        let token_count = tokens.unwrap_or_else(|| Self::estimate_tokens(&message.content));
        self.used_tokens += token_count;
        self.cumulative_tokens += token_count;
        self.conversation_history.push(message);

        debug!(
            "Added message with {} tokens (used: {}/{}, cumulative: {})",
            token_count, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// Update token usage from provider response
    pub fn update_usage_from_response(&mut self, usage: &g3_providers::Usage) {
        // Add the tokens from this response to our running total
        // The usage.total_tokens represents tokens used in this single API call
        self.used_tokens += usage.total_tokens;
        self.cumulative_tokens += usage.total_tokens;

        debug!(
            "Added {} tokens from provider response (used: {}/{}, cumulative: {})",
            usage.total_tokens, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    /// More accurate token estimation
    fn estimate_tokens(text: &str) -> u32 {
        // Better heuristic:
        // - Average English text: ~4 characters per token
        // - Code/JSON: ~3 characters per token (more symbols)
        // - Add 10% buffer for safety
        let base_estimate = if text.contains("{") || text.contains("```") || text.contains("fn ") {
            (text.len() as f32 / 3.0).ceil() as u32 // Code/JSON
        } else {
            (text.len() as f32 / 4.0).ceil() as u32 // Regular text
        };
        (base_estimate as f32 * 1.1).ceil() as u32 // Add 10% buffer
    }

    pub fn update_usage(&mut self, usage: &g3_providers::Usage) {
        // Deprecated: Use update_usage_from_response instead
        self.update_usage_from_response(usage);
    }

    /// Update cumulative token usage (for streaming)
    pub fn add_streaming_tokens(&mut self, new_tokens: u32) {
        self.used_tokens += new_tokens;
        self.cumulative_tokens += new_tokens;
        debug!(
            "Added {} streaming tokens (used: {}/{}, cumulative: {})",
            new_tokens, self.used_tokens, self.total_tokens, self.cumulative_tokens
        );
    }

    pub fn percentage_used(&self) -> f32 {
        if self.total_tokens == 0 {
            0.0
        } else {
            (self.used_tokens as f32 / self.total_tokens as f32) * 100.0
        }
    }

    pub fn remaining_tokens(&self) -> u32 {
        self.total_tokens.saturating_sub(self.used_tokens)
    }

    /// Check if we should trigger summarization (at 80% capacity)
    pub fn should_summarize(&self) -> bool {
        // Trigger at 80% OR if we're getting close to absolute limits
        // This prevents issues with models that have large contexts but still hit limits
        let percentage_trigger = self.percentage_used() >= 80.0;

        // Also trigger if we're approaching common token limits
        // Most models start having issues around 150k tokens
        let absolute_trigger = self.used_tokens > 150_000;

        percentage_trigger || absolute_trigger
    }

    /// Create a summary request prompt for the current conversation
    pub fn create_summary_prompt(&self) -> String {
        "Please provide a comprehensive summary of our conversation so far. Include:

1. **Main Topic/Goal**: What is the primary task or objective being worked on?
2. **Key Decisions**: What important decisions have been made?
3. **Actions Taken**: What specific actions, commands, or code changes have been completed?
4. **Current State**: What is the current status of the work?
5. **Important Context**: Any critical information, file paths, configurations, or constraints that should be remembered?
6. **Pending Items**: What remains to be done or what was the user's last request?

Format this as a detailed but concise summary that can be used to resume the conversation from scratch while maintaining full context.".to_string()
    }

    /// Reset the context window with a summary
    pub fn reset_with_summary(&mut self, summary: String, latest_user_message: Option<String>) {
        // Clear the conversation history
        self.conversation_history.clear();
        self.used_tokens = 0;

        // Add the summary as a system message
        let summary_message = Message {
            role: MessageRole::System,
            content: format!("Previous conversation summary:\n\n{}", summary),
        };
        self.add_message(summary_message);

        // Add the latest user message if provided
        if let Some(user_msg) = latest_user_message {
            self.add_message(Message {
                role: MessageRole::User,
                content: user_msg,
            });
        }
    }
}

pub struct Agent<W: UiWriter> {
    providers: ProviderRegistry,
    context_window: ContextWindow,
    config: Config,
    session_id: Option<String>,
    tool_call_metrics: Vec<(String, Duration, bool)>, // (tool_name, duration, success)
    ui_writer: W,
    is_autonomous: bool,
    quiet: bool,
    computer_controller: Option<Box<dyn g3_computer_control::ComputerController>>,
    todo_content: std::sync::Arc<tokio::sync::RwLock<String>>,
}

impl<W: UiWriter> Agent<W> {
    pub async fn new(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, false, false).await
    }

    pub async fn new_with_readme(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, false).await
    }

    pub async fn new_autonomous_with_readme(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, false).await
    }

    pub async fn new_autonomous(config: Config, ui_writer: W) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, true, false).await
    }

    pub async fn new_with_quiet(config: Config, ui_writer: W, quiet: bool) -> Result<Self> {
        Self::new_with_mode(config, ui_writer, false, quiet).await
    }

    pub async fn new_with_readme_and_quiet(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, false, readme_content, quiet).await
    }

    pub async fn new_autonomous_with_readme_and_quiet(
        config: Config,
        ui_writer: W,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, true, readme_content, quiet).await
    }

    async fn new_with_mode(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        quiet: bool,
    ) -> Result<Self> {
        Self::new_with_mode_and_readme(config, ui_writer, is_autonomous, None, quiet).await
    }

    async fn new_with_mode_and_readme(
        config: Config,
        ui_writer: W,
        is_autonomous: bool,
        readme_content: Option<String>,
        quiet: bool,
    ) -> Result<Self> {
        let mut providers = ProviderRegistry::new();

        // Only register providers that are configured AND selected as the default provider
        // This prevents unnecessary initialization of heavy providers like embedded models

        // Register embedded provider if configured AND it's the default provider
        if let Some(embedded_config) = &config.providers.embedded {
            if config.providers.default_provider == "embedded" {
                info!("Initializing embedded provider (selected as default)");
                let embedded_provider = g3_providers::EmbeddedProvider::new(
                    embedded_config.model_path.clone(),
                    embedded_config.model_type.clone(),
                    embedded_config.context_length,
                    embedded_config.max_tokens,
                    embedded_config.temperature,
                    embedded_config.gpu_layers,
                    embedded_config.threads,
                )?;
                providers.register(embedded_provider);
            } else {
                info!("Embedded provider configured but not selected as default, skipping initialization");
            }
        }

        // Register Anthropic provider if configured AND it's the default provider
        if let Some(anthropic_config) = &config.providers.anthropic {
            if config.providers.default_provider == "anthropic" {
                info!("Initializing Anthropic provider (selected as default)");
                let anthropic_provider = g3_providers::AnthropicProvider::new(
                    anthropic_config.api_key.clone(),
                    Some(anthropic_config.model.clone()),
                    anthropic_config.max_tokens,
                    anthropic_config.temperature,
                )?;
                providers.register(anthropic_provider);
            } else {
                info!("Anthropic provider configured but not selected as default, skipping initialization");
            }
        }

        // Register Databricks provider if configured AND it's the default provider
        if let Some(databricks_config) = &config.providers.databricks {
            if config.providers.default_provider == "databricks" {
                info!("Initializing Databricks provider (selected as default)");

                let databricks_provider = if let Some(token) = &databricks_config.token {
                    // Use token-based authentication
                    g3_providers::DatabricksProvider::from_token(
                        databricks_config.host.clone(),
                        token.clone(),
                        databricks_config.model.clone(),
                        databricks_config.max_tokens,
                        databricks_config.temperature,
                    )?
                } else {
                    // Use OAuth authentication
                    g3_providers::DatabricksProvider::from_oauth(
                        databricks_config.host.clone(),
                        databricks_config.model.clone(),
                        databricks_config.max_tokens,
                        databricks_config.temperature,
                    )
                    .await?
                };

                providers.register(databricks_provider);
            } else {
                info!("Databricks provider configured but not selected as default, skipping initialization");
            }
        }

        // Set default provider
        debug!(
            "Setting default provider to: {}",
            config.providers.default_provider
        );
        providers.set_default(&config.providers.default_provider)?;
        debug!("Default provider set successfully");

        // Determine context window size based on active provider
        let context_length = Self::determine_context_length(&config, &providers)?;
        let mut context_window = ContextWindow::new(context_length);

        // If README content is provided, add it as the first system message
        if let Some(readme) = readme_content {
            let readme_message = Message {
                role: MessageRole::System,
                content: readme,
            };
            context_window.add_message(readme_message);
            info!("Added project README to context window");
        }

        // Initialize computer controller if enabled
        let computer_controller = if config.computer_control.enabled {
            match g3_computer_control::create_controller() {
                Ok(controller) => {
                    info!("Computer control enabled");
                    Some(controller)
                }
                Err(e) => {
                    warn!("Failed to initialize computer control: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            providers,
            context_window,
            config,
            session_id: None,
            tool_call_metrics: Vec::new(),
            ui_writer,
            todo_content: std::sync::Arc::new(tokio::sync::RwLock::new(String::new())),
            is_autonomous,
            quiet,
            computer_controller,
        })
    }

    fn determine_context_length(config: &Config, providers: &ProviderRegistry) -> Result<u32> {
        // Get the active provider to determine context length
        let provider = providers.get(None)?;
        let provider_name = provider.name();
        let model_name = provider.model();

        // Use provider-specific context length if available, otherwise fall back to agent config
        let context_length = match provider_name {
            "embedded" => {
                // For embedded models, use the configured context_length or model-specific defaults
                if let Some(embedded_config) = &config.providers.embedded {
                    embedded_config.context_length.unwrap_or_else(|| {
                        // Model-specific defaults for embedded models
                        match embedded_config.model_type.to_lowercase().as_str() {
                            "codellama" => 16384, // CodeLlama supports 16k context
                            "llama" => 4096,      // Base Llama models
                            "mistral" => 8192,    // Mistral models
                            "qwen" => 32768,      // Qwen2.5 supports 32k context
                            _ => 4096,            // Conservative default
                        }
                    })
                } else {
                    config.agent.max_context_length as u32
                }
            }
            "anthropic" => {
                // Claude models have large context windows
                200000 // Default for Claude models
            }
            "databricks" => {
                // Databricks models have varying context windows depending on the model
                if model_name.contains("claude") {
                    200000 // Claude models on Databricks have large context windows
                } else if model_name.contains("llama") {
                    32768 // Llama models typically support 32k context
                } else if model_name.contains("dbrx") {
                    32768 // DBRX supports 32k context
                } else {
                    16384 // Conservative default for other Databricks models
                }
            }
            _ => config.agent.max_context_length as u32,
        };

        info!(
            "Using context length: {} tokens for provider: {} (model: {})",
            context_length, provider_name, model_name
        );

        Ok(context_length)
    }

    pub fn get_provider_info(&self) -> Result<(String, String)> {
        let provider = self.providers.get(None)?;
        Ok((provider.name().to_string(), provider.model().to_string()))
    }

    /// Get the current session ID for this agent
    pub fn get_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub async fn execute_task(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
    ) -> Result<TaskResult> {
        self.execute_task_with_options(description, language, false, false, false)
            .await
    }

    pub async fn execute_task_with_options(
        &mut self,
        description: &str,
        language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
    ) -> Result<TaskResult> {
        self.execute_task_with_timing(
            description,
            language,
            _auto_execute,
            show_prompt,
            show_code,
            false,
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
        )
        .await
    }

    pub async fn execute_task_with_timing_cancellable(
        &mut self,
        description: &str,
        _language: Option<&str>,
        _auto_execute: bool,
        show_prompt: bool,
        show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
    ) -> Result<TaskResult> {
        // Execute the task directly without splitting
        self.execute_single_task(
            description,
            show_prompt,
            show_code,
            show_timing,
            cancellation_token,
        )
        .await
    }

    async fn execute_single_task(
        &mut self,
        description: &str,
        show_prompt: bool,
        _show_code: bool,
        show_timing: bool,
        cancellation_token: CancellationToken,
    ) -> Result<TaskResult> {
        // Reset the JSON tool call filter state at the start of each new task
        // This prevents the filter from staying in suppression mode between user interactions
        fixed_filter_json::reset_fixed_json_tool_state();

        // Generate session ID based on the initial prompt if this is a new session
        if self.session_id.is_none() {
            self.session_id = Some(self.generate_session_id(description));
        }

        // Only add system message if this is the first interaction (empty conversation history)
        if self.context_window.conversation_history.is_empty() {
            let provider = self.providers.get(None)?;
            let system_prompt = if provider.has_native_tool_calling() {
                // For native tool calling providers, use a more explicit system prompt
                "You are G3, an AI programming agent of the same skill level as a seasoned engineer at a major technology company. You analyze given tasks and write code to achieve goals.

# Task Management

Use todo_read and todo_write for tasks with 3+ steps, multiple files/components, or uncertain scope.

Workflow:
- Start: read → write checklist
- During: read → update progress
- End: verify all complete

Warning: todo_write overwrites entirely; always todo_read first (skipping is an error)

Keep items short, specific, action-oriented. Not using the todo tools for complex tasks is an error.

Template:
- [ ] Implement feature X
  - [ ] Update API
  - [ ] Write tests
  - [ ] Run tests
  - [ ] Run lint
- [ ] Blocked: waiting on credentials

You have access to tools. When you need to accomplish a task, you MUST use the appropriate tool. Do not just describe what you would do - actually use the tools.

IMPORTANT: You must call tools to achieve goals. When you receive a request:
1. Analyze and identify what needs to be done
2. Call the appropriate tool with the required parameters
3. Continue or complete the task based on the result
4. If you repeatedly try something and it fails, try a different approach
5. Call the final_output tool with a detailed summary when done.

For shell commands: Use the shell tool with the exact command needed. Avoid commands that produce a large amount of output, and consider piping those outputs to files. Example: If asked to list files, immediately call the shell tool with command parameter \"ls\".
If you create temporary files for verification, place these in a subdir named 'tmp'. Do NOT pollute the current dir.

IMPORTANT: If the user asks you to just respond with text (like \"just say hello\" or \"tell me about X\"), do NOT use tools. Simply respond with the requested text directly. Only use tools when you need to execute commands or complete tasks that require action.

When taking screenshots of specific windows (like \"my Safari window\" or \"my terminal\"), ALWAYS use list_windows first to identify the correct window ID, then use take_screenshot with the window_id parameter.

Do not explain what you're going to do - just do it by calling the tools.

# Response Guidelines

- Use Markdown formatting for all responses except tool calls.
- Whenever taking actions, use the pronoun 'I'
".to_string()
            } else {
                // For non-native providers (embedded models), use JSON format instructions
                "You are G3, a general-purpose AI agent. Your goal is to analyze and solve problems by writing code.

# Task Management

Use todo_read and todo_write for tasks with 3+ steps, multiple files/components, or uncertain scope.

Workflow:
- Start: read → write checklist
- During: read → update progress
- End: verify all complete

Warning: todo_write overwrites entirely; always todo_read first (skipping is an error)

Keep items short, specific, action-oriented. Not using the todo tools for complex tasks is an error.

Template:
- [ ] Implement feature X
  - [ ] Update API
  - [ ] Write tests

# Tool Call Format

When you need to execute a tool, write ONLY the JSON tool call on a new line:

{\"tool\": \"tool_name\", \"args\": {\"param\": \"value\"}

The tool will execute immediately and you'll receive the result (success or error) to continue with.

# Available Tools

- **shell**: Execute shell commands
  - Format: {\"tool\": \"shell\", \"args\": {\"command\": \"your_command_here\"}
  - Example: {\"tool\": \"shell\", \"args\": {\"command\": \"ls ~/Downloads\"}

- **read_file**: Read the contents of a file (supports partial reads via start/end)
  - Format: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"path/to/file\", \"start\": 0, \"end\": 100}
  - Example: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"src/main.rs\"}
  - Example (partial): {\"tool\": \"read_file\", \"args\": {\"file_path\": \"large.log\", \"start\": 0, \"end\": 1000}

- **write_file**: Write content to a file (creates or overwrites)
  - Format: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"path/to/file\", \"content\": \"file content\"}
  - Example: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"src/lib.rs\", \"content\": \"pub fn hello() {}\"}

- **str_replace**: Replace text in a file using a diff
  - Format: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"path/to/file\", \"diff\": \"--- old\\n-old text\\n+++ new\\n+new text\"}
  - Example: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"src/main.rs\", \"diff\": \"--- old\\n-old_code();\\n+++ new\\n+new_code();\"}

- **final_output**: Signal task completion with a detailed summary of work done in markdown format
  - Format: {\"tool\": \"final_output\", \"args\": {\"summary\": \"what_was_accomplished\"}

- **todo_read**: Read the entire TODO list content
  - Format: {\"tool\": \"todo_read\", \"args\": {}}
  - Example: {\"tool\": \"todo_read\", \"args\": {}}

- **todo_write**: Write or overwrite the entire TODO list (WARNING: overwrites completely, always read first)
  - Format: {\"tool\": \"todo_write\", \"args\": {\"content\": \"- [ ] Task 1\\n- [ ] Task 2\"}}
  - Example: {\"tool\": \"todo_write\", \"args\": {\"content\": \"- [ ] Implement feature\\n  - [ ] Write tests\\n  - [ ] Run tests\"}}

# Instructions

1. Analyze the request and break down into smaller tasks if appropriate
2. Execute ONE tool at a time
3. STOP when the original request was satisfied
4. Call the final_output tool when done

# Response Guidelines

- Use Markdown formatting for all responses except tool calls.
- Whenever taking actions, use the pronoun 'I'

".to_string()
            };

            if show_prompt {
                self.ui_writer.print_system_prompt(&system_prompt);
            }

            // Add system message to context window
            let system_message = Message {
                role: MessageRole::System,
                content: system_prompt,
            };
            self.context_window.add_message(system_message);
        }

        // Add user message to context window
        let user_message = Message {
            role: MessageRole::User,
            content: format!("Task: {}", description),
        };
        self.context_window.add_message(user_message);

        // Use the complete conversation history for the request
        let messages = self.context_window.conversation_history.clone();

        // Check if provider supports native tool calling and add tools if so
        let provider = self.providers.get(None)?;
        let tools = if provider.has_native_tool_calling() {
            Some(Self::create_tool_definitions())
        } else {
            None
        };

        // Get max_tokens from provider configuration
        // For Databricks, this should be much higher to support large file generation
        let max_tokens = match provider.name() {
            "databricks" => {
                // Use the model's maximum limit for Databricks to allow large file generation
                Some(32000)
            }
            _ => {
                // Default for other providers
                Some(16000)
            }
        };

        let request = CompletionRequest {
            messages,
            max_tokens,
            temperature: Some(0.1),
            stream: true, // Enable streaming
            tools,
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

        // Create a mock usage for now (we'll need to track this during streaming)
        let mock_usage = g3_providers::Usage {
            prompt_tokens: 100,                                   // Estimate
            completion_tokens: response_content.len() as u32 / 4, // Rough estimate
            total_tokens: 100 + (response_content.len() as u32 / 4),
        };

        // Update context window with estimated token usage
        self.context_window.update_usage(&mock_usage);

        // Add assistant response to context window only if not empty
        // This prevents the "Skipping empty message" warning when only tools were executed
        if !response_content.trim().is_empty() {
            let assistant_message = Message {
                role: MessageRole::Assistant,
                content: response_content.clone(),
            };
            self.context_window.add_message(assistant_message);
        } else {
            debug!("Assistant response was empty (likely only tool execution), skipping message addition");
        }

        // Save context window at the end of successful interaction
        self.save_context_window("completed");

        // Return the task result which already includes timing if needed
        Ok(task_result)
    }

    /// Generate a session ID based on the initial prompt
    fn generate_session_id(&self, description: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Clean and truncate the description for a readable filename
        let clean_description = description
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-' || *c == '_')
            .collect::<String>()
            .split_whitespace()
            .take(5) // Take first 5 words
            .collect::<Vec<_>>()
            .join("_")
            .to_lowercase();

        // Create a hash for uniqueness
        let mut hasher = DefaultHasher::new();
        description.hash(&mut hasher);
        let hash = hasher.finish();

        // Format: clean_description_hash
        format!("{}_{:x}", clean_description, hash)
    }

    /// Save the entire context window to a per-session file
    fn save_context_window(&self, status: &str) {
        // Skip logging if quiet mode is enabled
        if self.quiet {
            return;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Create logs directory if it doesn't exist
        let logs_dir = std::path::Path::new("logs");
        if !logs_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(logs_dir) {
                error!("Failed to create logs directory: {}", e);
                return;
            }
        }

        // Use session-based filename if we have a session ID, otherwise fall back to timestamp
        let filename = if let Some(ref session_id) = self.session_id {
            format!("logs/g3_session_{}.json", session_id)
        } else {
            format!("logs/g3_context_{}.json", timestamp)
        };

        let context_data = serde_json::json!({
            "session_id": self.session_id,
            "timestamp": timestamp,
            "status": status,
            "context_window": {
                "used_tokens": self.context_window.used_tokens,
                "total_tokens": self.context_window.total_tokens,
                "percentage_used": self.context_window.percentage_used(),
                "conversation_history": self.context_window.conversation_history
            }
        });

        match serde_json::to_string_pretty(&context_data) {
            Ok(json_content) => {
                if let Err(e) = std::fs::write(&filename, json_content) {
                    error!("Failed to save context window to {}: {}", filename, e);
                }
            }
            Err(e) => {
                error!("Failed to serialize context window: {}", e);
            }
        }
    }

    pub fn get_context_window(&self) -> &ContextWindow {
        &self.context_window
    }

    pub fn get_tool_call_metrics(&self) -> &Vec<(String, Duration, bool)> {
        &self.tool_call_metrics
    }

    pub fn get_config(&self) -> &Config {
        &self.config
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
    fn create_tool_definitions() -> Vec<Tool> {
        vec![
            Tool {
                name: "shell".to_string(),
                description: "Execute shell commands".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
            Tool {
                name: "read_file".to_string(),
                description: "Read the contents of a file. For image files (png, jpg, jpeg, gif, bmp, tiff, webp), automatically extracts text using OCR. For text files, optionally read a specific character range.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "start": {
                            "type": "integer",
                            "description": "Starting character position (0-indexed, inclusive). If omitted, reads from beginning."
                        },
                        "end": {
                            "type": "integer",
                            "description": "Ending character position (0-indexed, EXCLUSIVE). If omitted, reads to end of file."
                        }
                    },
                    "required": ["file_path"]
                }),
            },
            Tool {
                name: "write_file".to_string(),
                description: "Write content to a file (creates or overwrites). You MUST provide all arguments".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["file_path", "content"]
                }),
            },
            Tool {
                name: "str_replace".to_string(),
                description: "Apply a unified diff to a file. Supports multiple hunks and context lines. Optionally constrain the search to a [start, end) character range (0-indexed; end is EXCLUSIVE). Useful to disambiguate matches or limit scope in large files.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to edit"
                        },
                        "diff": {
                            "type": "string",
                            "description": "A unified diff showing what to replace. Supports @@ hunk headers, context lines, and multiple hunks (---/+++ headers optional for minimal diffs)."
                        },
                        "start": {
                            "type": "integer",
                            "description": "Starting character position in the file (0-indexed, inclusive). If omitted, searches from beginning."
                        },
                        "end": {
                            "type": "integer",
                            "description": "Ending character position in the file (0-indexed, EXCLUSIVE - character at this position is NOT included). If omitted, searches to end of file."
                        }
                    },
                    "required": ["file_path", "diff"]
                }),
            },
            Tool {
                name: "final_output".to_string(),
                description: "Signal task completion with a detailed summary".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "A detailed summary in markdown of what was accomplished"
                        }
                    },
                    "required": ["summary"]
                }),
            },
            Tool {
                name: "mouse_click".to_string(),
                description: "Click the mouse at specific coordinates".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "x": {
                            "type": "integer",
                            "description": "X coordinate"
                        },
                        "y": {
                            "type": "integer",
                            "description": "Y coordinate"
                        },
                        "button": {
                            "type": "string",
                            "enum": ["left", "right", "middle"],
                            "description": "Mouse button to click",
                            "default": "left"
                        }
                    },
                    "required": ["x", "y"]
                }),
            },
            Tool {
                name: "type_text".to_string(),
                description: "Type text at the current cursor position".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Text to type"
                        }
                    },
                    "required": ["text"]
                }),
            },
            Tool {
                name: "find_element".to_string(),
                description: "Find a UI element by text, role, or other attributes".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Text to search for"
                        },
                        "role": {
                            "type": "string",
                            "description": "Element role (button, textfield, etc.)"
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Optional window ID to search in"
                        }
                    }
                }),
            },
            Tool {
                name: "take_screenshot".to_string(),
                description: "Capture a screenshot of the screen, region, or window. When capturing a specific application window (e.g., 'Safari', 'Terminal'), use the window_id parameter with just the application name. The tool will automatically use the native screencapture command with the application's window ID for a clean capture.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Filename for the screenshot (e.g., 'safari.png'). If a relative path is provided, the screenshot will be saved to ~/tmp or $TMPDIR. Use an absolute path to save elsewhere."
                        },
                        "window_id": {
                            "type": "string",
                            "description": "Optional application name to capture (e.g., 'Safari', 'Terminal', 'Google Chrome'). The tool will capture the frontmost window of that application using its native window ID."
                        },
                        "region": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "integer"},
                                "y": {"type": "integer"},
                                "width": {"type": "integer"},
                                "height": {"type": "integer"}
                            }
                        }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "extract_text".to_string(),
                description: "Extract text from a screen region or image file using OCR".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to image file (optional if region is provided)"
                        },
                        "region": {
                            "type": "object",
                            "description": "Screen region to capture and extract text from",
                            "properties": {
                                "x": {"type": "integer"},
                                "y": {"type": "integer"},
                                "width": {"type": "integer"},
                                "height": {"type": "integer"}
                            }
                        }
                    }
                }),
            },
            Tool {
                name: "find_text_on_screen".to_string(),
                description: "Find text visually on screen and return its coordinates".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Text to search for on screen"
                        }
                    },
                    "required": ["text"]
                }),
            },
            Tool {
                name: "list_windows".to_string(),
                description: "List all currently open windows with their IDs, titles, and application names. Use this to identify which window to interact with before taking screenshots or performing other window-specific operations.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            Tool {
                name: "todo_read".to_string(),
                description: "Read the entire TODO list content. Use this to view current tasks, notes, and any other information stored in the TODO list.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
            Tool {
                name: "todo_write".to_string(),
                description: "Write or overwrite the entire TODO list content. This tool replaces the complete TODO list with the provided string. Use this to update tasks, add new items, or reorganize the TODO list. WARNING: This operation completely replaces the TODO list content. Make sure to include all content you want to keep, not just the changes.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The TODO list content to save. Use markdown checkbox format: - [ ] for incomplete tasks, - [x] for completed tasks. Support nested tasks with indentation."
                        }
                    },
                    "required": ["content"]
                }),
            },
        ]
    }

    /// Helper method to stream with retry logic
    async fn stream_with_retry(
        &self,
        request: &CompletionRequest,
        error_context: &error_handling::ErrorContext,
    ) -> Result<g3_providers::CompletionStream> {
        use crate::error_handling::{calculate_retry_delay, classify_error, ErrorType};

        let mut attempt = 0;
        let max_attempts = if self.is_autonomous { 6 } else { 3 };

        loop {
            attempt += 1;
            let provider = self.providers.get(None)?;

            match provider.stream(request.clone()).await {
                Ok(stream) => {
                    if attempt > 1 {
                        info!("Stream started successfully after {} attempts", attempt);
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
        use crate::error_handling::ErrorContext;
        use tokio_stream::StreamExt;

        debug!("Starting stream_completion_with_tools");

        let mut full_response = String::new();
        let mut first_token_time: Option<Duration> = None;
        let stream_start = Instant::now();
        let mut iteration_count = 0;
        const MAX_ITERATIONS: usize = 400; // Prevent infinite loops
        let mut response_started = false;

        // Check if we need to summarize before starting
        if self.context_window.should_summarize() {
            info!(
                "Context window at {}% ({}/{} tokens), triggering auto-summarization",
                self.context_window.percentage_used() as u32,
                self.context_window.used_tokens,
                self.context_window.total_tokens
            );

            // Notify user about summarization
            self.ui_writer.print_context_status(&format!(
                "\n📊 Context window reaching capacity ({}%). Creating summary...",
                self.context_window.percentage_used() as u32
            ));

            // Create summary request with FULL history
            let summary_prompt = self.context_window.create_summary_prompt();

            // Get the full conversation history
            let conversation_text = self
                .context_window
                .conversation_history
                .iter()
                .map(|m| format!("{:?}: {}", m.role, m.content))
                .collect::<Vec<_>>()
                .join("\n\n");

            let summary_messages = vec![
                Message {
                    role: MessageRole::System,
                    content: "You are a helpful assistant that creates concise summaries."
                        .to_string(),
                },
                Message {
                    role: MessageRole::User,
                    content: format!(
                        "Based on this conversation history, {}\n\nConversation:\n{}",
                        summary_prompt, conversation_text
                    ),
                },
            ];

            let provider = self.providers.get(None)?;

            // Dynamically calculate max_tokens for summary based on what's left
            // We need to ensure: used_tokens + max_tokens <= total_context_limit
            let summary_max_tokens = match provider.name() {
                "databricks" | "anthropic" => {
                    // Claude models have 200k context
                    // Calculate how much room we have left
                    let model_limit = 200_000u32;
                    let current_usage = self.context_window.used_tokens;
                    // Leave some buffer (5k tokens) for safety
                    let available = model_limit
                        .saturating_sub(current_usage)
                        .saturating_sub(5000);
                    // Cap at a reasonable summary size (10k tokens max)
                    Some(available.min(10_000))
                }
                "embedded" => {
                    // For smaller context models, be more conservative
                    let model_limit = self.context_window.total_tokens;
                    let current_usage = self.context_window.used_tokens;
                    // Leave 1k buffer
                    let available = model_limit
                        .saturating_sub(current_usage)
                        .saturating_sub(1000);
                    // Cap at 3k for embedded models
                    Some(available.min(3000))
                }
                _ => {
                    // Default: conservative approach
                    let available = self.context_window.remaining_tokens().saturating_sub(2000);
                    Some(available.min(5000))
                }
            };

            info!(
                "Requesting summary with max_tokens: {:?} (current usage: {} tokens)",
                summary_max_tokens, self.context_window.used_tokens
            );

            let summary_request = CompletionRequest {
                messages: summary_messages,
                max_tokens: summary_max_tokens,
                temperature: Some(0.3), // Lower temperature for factual summary
                stream: false,
                tools: None,
            };

            // Get the summary
            match provider.complete(summary_request).await {
                Ok(summary_response) => {
                    self.ui_writer.print_context_status(
                        "✅ Summary created successfully. Resetting context window...\n",
                    );

                    // Extract the latest user message from the request
                    let latest_user_msg = request
                        .messages
                        .iter()
                        .rev()
                        .find(|m| matches!(m.role, MessageRole::User))
                        .map(|m| m.content.clone());

                    // Reset context with summary
                    self.context_window
                        .reset_with_summary(summary_response.content, latest_user_msg);

                    // Update the request with new context
                    request.messages = self.context_window.conversation_history.clone();

                    self.ui_writer.print_context_status(
                        "🔄 Context reset complete. Continuing with your request...\n",
                    );
                }
                Err(e) => {
                    error!("Failed to create summary: {}", e);
                    self.ui_writer.print_context_status("⚠️ Unable to create summary. Consider starting a new session if you continue to see errors.\n");
                    // Don't continue with the original request if summarization failed
                    // as we're likely at token limit
                    return Err(anyhow::anyhow!("Context window at capacity and summarization failed. Please start a new session."));
                }
            }
        }

        loop {
            iteration_count += 1;
            debug!("Starting iteration {}", iteration_count);
            if iteration_count > MAX_ITERATIONS {
                warn!("Maximum iterations reached, stopping stream");
                break;
            }

            // Add a small delay between iterations to prevent "model busy" errors
            if iteration_count > 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }

            let provider = self.providers.get(None)?;
            debug!("Got provider: {}", provider.name());

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
                provider.name().to_string(),
                provider.model().to_string(),
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
                provider.name(),
                provider.model(),
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
                    if iteration_count > 1 && e.to_string().contains("busy") {
                        warn!(
                            "Model busy on iteration {}, attempting one more retry in 500ms",
                            iteration_count
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

            let mut parser = StreamingToolParser::new();
            let mut current_response = String::new();
            let mut tool_executed = false;
            let mut chunks_received = 0;
            let mut raw_chunks: Vec<String> = Vec::new(); // Store raw chunks for debugging
            let mut _last_error: Option<String> = None;
            let mut accumulated_usage: Option<g3_providers::Usage> = None;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Notify UI about SSE received (including pings)
                        self.ui_writer.notify_sse_received();

                        // Capture usage data if available
                        if let Some(ref usage) = chunk.usage {
                            accumulated_usage = Some(usage.clone());
                            debug!(
                                "Received usage data - prompt: {}, completion: {}, total: {}",
                                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                            );
                        }

                        // Store raw chunk for debugging (limit to first 20 and last 5)
                        if chunks_received < 20 || chunk.finished {
                            raw_chunks.push(format!(
                                "Chunk #{}: content={:?}, finished={}, tool_calls={:?}",
                                chunks_received + 1,
                                chunk.content,
                                chunk.finished,
                                chunk.tool_calls
                            ));
                        } else if raw_chunks.len() == 20 {
                            raw_chunks.push("... (chunks 21+ omitted for brevity) ...".to_string());
                        }

                        // Record time to first token
                        if first_token_time.is_none() && !chunk.content.is_empty() {
                            first_token_time = Some(stream_start.elapsed());
                        }

                        chunks_received += 1;
                        if chunks_received == 1 {
                            debug!(
                                "First chunk received: content_len={}, finished={}",
                                chunk.content.len(),
                                chunk.finished
                            );
                        }

                        // Process chunk with the new parser
                        let completed_tools = parser.process_chunk(&chunk);

                        // Handle completed tool calls
                        for tool_call in completed_tools {
                            debug!("Processing completed tool call: {:?}", tool_call);

                            // Track what we've already displayed before getting new text
                            // This prevents re-displaying old content after tool execution
                            let already_displayed_chars = current_response.chars().count();

                            // Get the text content accumulated so far
                            let text_content = parser.get_text_content();

                            // Clean the content
                            let clean_content = text_content
                                .replace("<|im_end|>", "")
                                .replace("</s>", "")
                                .replace("[/INST]", "")
                                .replace("<</SYS>>", "");

                            // Filter out JSON tool calls from the display
                            let filtered_content =
                                fixed_filter_json::fixed_filter_json_tool_calls(&clean_content);
                            let final_display_content = filtered_content.trim();

                            // Display any new content before tool execution
                            // We need to skip what was already shown (tracked in current_response)
                            // but also account for the fact that parser.text_buffer accumulates
                            // across iterations and is never cleared until reset()
                            let new_content =
                                if current_response.len() <= final_display_content.len() {
                                    // Only show content that hasn't been displayed yet
                                    final_display_content
                                        .chars()
                                        .skip(already_displayed_chars)
                                        .collect::<String>()
                                } else {
                                    // Nothing new to display
                                    String::new()
                                };

                            if !new_content.trim().is_empty() {
                                #[allow(unused_assignments)]
                                if !response_started {
                                    self.ui_writer.print_agent_prompt();
                                    response_started = true;
                                }
                                self.ui_writer.print_agent_response(&new_content);
                                self.ui_writer.flush();
                                // Update current_response to track what we've displayed
                                current_response.push_str(&new_content);
                            }

                            // Execute the tool with formatted output
                            self.ui_writer.println(""); // New line before tool execution

                            // Skip printing tool call details for final_output
                            if tool_call.tool != "final_output" {
                                // Tool call header
                                self.ui_writer.print_tool_header(&tool_call.tool);
                                if let Some(args_obj) = tool_call.args.as_object() {
                                    for (key, value) in args_obj {
                                        let value_str = match value {
                                            serde_json::Value::String(s) => {
                                                if tool_call.tool == "shell" && key == "command" {
                                                    if let Some(first_line) = s.lines().next() {
                                                        if s.lines().count() > 1 {
                                                            format!("{}...", first_line)
                                                        } else {
                                                            first_line.to_string()
                                                        }
                                                    } else {
                                                        s.clone()
                                                    }
                                                } else {
                                                    if s.len() > 100 {
                                                        // Use char_indices to respect UTF-8 boundaries
                                                        let truncated = s
                                                            .char_indices()
                                                            .take(100)
                                                            .map(|(_, c)| c)
                                                            .collect::<String>();
                                                        format!("{}...", truncated)
                                                    } else {
                                                        s.clone()
                                                    }
                                                }
                                            }
                                            _ => value.to_string(),
                                        };
                                        self.ui_writer.print_tool_arg(key, &value_str);
                                    }
                                }
                                self.ui_writer.print_tool_output_header();
                            }

                            let exec_start = Instant::now();
                            // Add 8-minute timeout for tool execution
                            let tool_result = match tokio::time::timeout(
                                Duration::from_secs(8 * 60), // 8 minutes
                                self.execute_tool(&tool_call),
                            )
                            .await
                            {
                                Ok(result) => result?,
                                Err(_) => {
                                    warn!("Tool call {} timed out after 8 minutes", tool_call.tool);
                                    format!("❌ Tool execution timed out after 8 minutes")
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
                            if tool_call.tool != "final_output" {
                                let output_lines: Vec<&str> = tool_result.lines().collect();

                                // Helper function to safely truncate strings at character boundaries
                                let truncate_line = |line: &str, max_width: usize| -> String {
                                    let char_count = line.chars().count();
                                    if char_count <= max_width {
                                        line.to_string()
                                    } else {
                                        let truncated: String = line
                                            .chars()
                                            .take(max_width.saturating_sub(3))
                                            .collect();
                                        format!("{}...", truncated)
                                    }
                                };

                                const MAX_LINES: usize = 5;
                                const MAX_LINE_WIDTH: usize = 80;
                                let output_len = output_lines.len();

                                for line in output_lines {
                                    // Clip line to max width
                                    let clipped_line = truncate_line(line, MAX_LINE_WIDTH);
                                    self.ui_writer.update_tool_output_line(&clipped_line);
                                }

                                if output_len > MAX_LINES {
                                    self.ui_writer.print_tool_output_summary(output_len);
                                }
                            }

                            // Check if this was a final_output tool call
                            if tool_call.tool == "final_output" {
                                full_response.push_str(final_display_content);
                                if let Some(summary) = tool_call.args.get("summary") {
                                    if let Some(summary_str) = summary.as_str() {
                                        full_response.push_str(&format!("\n\n{}", summary_str));
                                    }
                                }
                                self.ui_writer.println("");
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    format!(
                                        "{}\n\n🕝 {} | 💭 {}",
                                        full_response,
                                        Self::format_duration(stream_start.elapsed()),
                                        Self::format_duration(_ttft)
                                    )
                                } else {
                                    full_response
                                };

                                return Ok(TaskResult::new(
                                    final_response,
                                    self.context_window.clone(),
                                ));
                            }

                            // Closure marker with timing
                            if tool_call.tool != "final_output" {
                                self.ui_writer
                                    .print_tool_timing(&Self::format_duration(exec_duration));
                                self.ui_writer.print_agent_prompt();
                            }

                            // Add the tool call and result to the context window
                            // Only include the text content if it's not already in full_response
                            let tool_message = if !full_response.contains(final_display_content) {
                                Message {
                                    role: MessageRole::Assistant,
                                    content: format!(
                                        "{}\n\n{{\"tool\": \"{}\", \"args\": {}}}",
                                        final_display_content.trim(),
                                        tool_call.tool,
                                        tool_call.args
                                    ),
                                }
                            } else {
                                // If we've already added the text, just include the tool call
                                Message {
                                    role: MessageRole::Assistant,
                                    content: format!(
                                        "{{\"tool\": \"{}\", \"args\": {}}}",
                                        tool_call.tool, tool_call.args
                                    ),
                                }
                            };
                            let result_message = Message {
                                role: MessageRole::User,
                                content: format!("Tool result: {}", tool_result),
                            };

                            self.context_window.add_message(tool_message);
                            self.context_window.add_message(result_message);

                            // Update the request with the new context for next iteration
                            request.messages = self.context_window.conversation_history.clone();

                            // Ensure tools are included for native providers in subsequent iterations
                            if provider.has_native_tool_calling() {
                                request.tools = Some(Self::create_tool_definitions());
                            }

                            // Only add to full_response if we haven't already added it
                            if !full_response.contains(final_display_content) {
                                full_response.push_str(final_display_content);
                            }
                            tool_executed = true;

                            // Reset the JSON tool call filter state after each tool execution
                            // This ensures the filter doesn't stay in suppression mode for subsequent streaming content
                            fixed_filter_json::reset_fixed_json_tool_state();

                            // Reset parser for next iteration
                            parser.reset();
                            // Clear current_response for next iteration to prevent buffered text
                            // from being incorrectly displayed after tool execution
                            current_response.clear();
                            // Reset response_started flag for next iteration
                            response_started = false;
                            break; // Break out of current stream to start a new one
                        }

                        // If no tool calls were completed, continue streaming normally
                        if !tool_executed {
                            let clean_content = chunk
                                .content
                                .replace("<|im_end|>", "")
                                .replace("</s>", "")
                                .replace("[/INST]", "")
                                .replace("<</SYS>>", "");

                            if !clean_content.is_empty() {
                                let filtered_content =
                                    fixed_filter_json::fixed_filter_json_tool_calls(&clean_content);

                                if !filtered_content.is_empty() {
                                    if !response_started {
                                        self.ui_writer.print_agent_prompt();
                                        response_started = true;
                                    }

                                    self.ui_writer.print_agent_response(&filtered_content);
                                    self.ui_writer.flush();
                                    current_response.push_str(&filtered_content);
                                }
                            }
                        }

                        if chunk.finished {
                            debug!("Stream finished: tool_executed={}, current_response_len={}, full_response_len={}, chunks_received={}",
                                tool_executed, current_response.len(), full_response.len(), chunks_received);

                            // Stream finished - check if we should continue or return
                            if !tool_executed {
                                // No tools were executed in this iteration
                                // Check if we got any meaningful response at all
                                // We need to check the parser's text buffer as well, since the LLM
                                // might have responded with text but no final_output tool call
                                let text_content = parser.get_text_content();
                                let has_text_response = !text_content.trim().is_empty()
                                    || !current_response.trim().is_empty();

                                // Don't re-add text from parser buffer if we already displayed it
                                // The parser buffer contains ALL accumulated text, but current_response
                                // already has what was displayed during streaming
                                if current_response.is_empty() && !text_content.trim().is_empty() {
                                    // Only use parser text if we truly have no response
                                    // This should be rare - only if streaming failed to display anything
                                    debug!("Warning: Using parser buffer text as fallback - this may duplicate output");
                                    // Extract only the undisplayed portion from parser buffer
                                    // Parser buffer accumulates across iterations, so we need to be careful
                                    let clean_text = text_content
                                        .replace("<|im_end|>", "")
                                        .replace("</s>", "")
                                        .replace("[/INST]", "")
                                        .replace("<</SYS>>", "");

                                    let filtered_text =
                                        fixed_filter_json::fixed_filter_json_tool_calls(
                                            &clean_text,
                                        );

                                    // Only use this if we truly have nothing else
                                    if !filtered_text.trim().is_empty() && full_response.is_empty()
                                    {
                                        debug!(
                                            "Using filtered parser text as last resort: {} chars",
                                            filtered_text.len()
                                        );
                                        current_response = filtered_text;
                                    }
                                }

                                if !has_text_response && full_response.is_empty() {
                                    // Log detailed error information before failing
                                    error!(
                                        "=== STREAM ERROR: No content or tool calls received ==="
                                    );
                                    error!("Iteration: {}/{}", iteration_count, MAX_ITERATIONS);
                                    error!(
                                        "Provider: {} (model: {})",
                                        provider.name(),
                                        provider.model()
                                    );
                                    error!("Chunks received: {}", chunks_received);
                                    error!("Parser state:");
                                    error!("  - Text buffer length: {}", parser.text_buffer_len());
                                    error!(
                                        "  - Text buffer content: {:?}",
                                        parser.get_text_content()
                                    );
                                    error!("  - Native tool calls: {:?}", parser.native_tool_calls);
                                    error!("  - Message stopped: {}", parser.is_message_stopped());
                                    error!("  - In JSON tool call: {}", parser.in_json_tool_call);
                                    error!("  - JSON tool start: {:?}", parser.json_tool_start);
                                    error!("Request details:");
                                    error!("  - Messages count: {}", request.messages.len());
                                    error!("  - Has tools: {}", request.tools.is_some());
                                    error!("  - Max tokens: {:?}", request.max_tokens);
                                    error!("  - Temperature: {:?}", request.temperature);
                                    error!("  - Stream: {}", request.stream);

                                    // Log raw chunks received
                                    error!("Raw chunks received ({} total):", chunks_received);
                                    for (i, chunk_str) in raw_chunks.iter().take(25).enumerate() {
                                        error!("  [{}] {}", i, chunk_str);
                                    }

                                    // Log the full request JSON
                                    match serde_json::to_string_pretty(&request) {
                                        Ok(json) => {
                                            error!(
                                                "(turn on DEBUG logging for the raw JSON request)"
                                            );
                                            debug!("Full request JSON:\n{}", json);
                                        }
                                        Err(e) => {
                                            error!("Failed to serialize request: {}", e);
                                        }
                                    }

                                    // Log last user message for context
                                    if let Some(last_user_msg) = request
                                        .messages
                                        .iter()
                                        .rev()
                                        .find(|m| matches!(m.role, MessageRole::User))
                                    {
                                        error!(
                                            "Last user message: {}",
                                            if last_user_msg.content.len() > 500 {
                                                format!(
                                                    "{}... (truncated)",
                                                    &last_user_msg.content[..500]
                                                )
                                            } else {
                                                last_user_msg.content.clone()
                                            }
                                        );
                                    }

                                    // Log context window state
                                    error!("Context window state:");
                                    error!(
                                        "  - Used tokens: {}/{}",
                                        self.context_window.used_tokens,
                                        self.context_window.total_tokens
                                    );
                                    error!(
                                        "  - Percentage used: {:.1}%",
                                        self.context_window.percentage_used()
                                    );
                                    error!(
                                        "  - Conversation history length: {}",
                                        self.context_window.conversation_history.len()
                                    );

                                    // Log session info
                                    error!("Session ID: {:?}", self.session_id);
                                    error!("=== END STREAM ERROR ===");

                                    // No response received - this is an error condition
                                    warn!("Stream finished without any content or tool calls");
                                    warn!("Chunks received: {}", chunks_received);
                                    return Err(anyhow::anyhow!(
                                        "No response received from the model. The model may be experiencing issues or the request may have been malformed."
                                    ));
                                }

                                // Set full_response to current_response (don't append)
                                // current_response already contains everything that was displayed
                                // Appending would duplicate the output
                                if !current_response.is_empty() && full_response.is_empty() {
                                    full_response = current_response.clone();
                                    debug!("Set full_response from current_response (no tool): {} chars", full_response.len());
                                }

                                self.ui_writer.println("");
                                let _ttft =
                                    first_token_time.unwrap_or_else(|| stream_start.elapsed());

                                // Add timing if needed
                                let final_response = if show_timing {
                                    format!(
                                        "{}\n\n⏱️ {} | 💭 {}",
                                        full_response,
                                        Self::format_duration(stream_start.elapsed()),
                                        Self::format_duration(_ttft)
                                    )
                                } else {
                                    full_response
                                };

                                return Ok(TaskResult::new(
                                    final_response,
                                    self.context_window.clone(),
                                ));
                            }
                            break; // Tool was executed, break to continue outer loop
                        }
                    }
                    Err(e) => {
                        // Capture detailed streaming error information
                        let error_details =
                            format!("Streaming error at chunk {}: {}", chunks_received + 1, e);
                        error!("{}", error_details);
                        error!("Error type: {}", std::any::type_name_of_val(&e));
                        error!("Parser state at error: text_buffer_len={}, native_tool_calls={}, message_stopped={}",
                            parser.text_buffer_len(), parser.native_tool_calls.len(), parser.is_message_stopped());

                        // Store the error for potential logging later
                        _last_error = Some(error_details);

                        if tool_executed {
                            warn!("Stream error after tool execution, attempting to continue");
                            break; // Break to outer loop to start new stream
                        } else {
                            // Log raw chunks before failing
                            error!("Fatal streaming error. Raw chunks received before error:");
                            for chunk_str in raw_chunks.iter().take(10) {
                                error!("  {}", chunk_str);
                            }
                            return Err(e);
                        }
                    }
                }
            }

            // Update context window with actual usage if available
            if let Some(usage) = accumulated_usage {
                debug!("Updating context window with actual usage from stream");
                self.context_window.update_usage_from_response(&usage);
            } else {
                // Fall back to estimation if no usage data was provided
                debug!("No usage data from stream, using estimation");
                let estimated_tokens = ContextWindow::estimate_tokens(&current_response);
                self.context_window.add_streaming_tokens(estimated_tokens);
            }

            // If we get here and no tool was executed, we're done
            if !tool_executed {
                // IMPORTANT: Do NOT add parser text_content here!
                // The text has already been displayed during streaming via current_response.
                // The parser buffer accumulates ALL text and would cause duplication.
                debug!("Stream completed without tool execution. Response already displayed during streaming.");
                debug!(
                    "Current response length: {}, Full response length: {}",
                    current_response.len(),
                    full_response.len()
                );

                let has_response = !current_response.is_empty() || !full_response.is_empty();

                if !has_response {
                    warn!(
                        "Loop exited without any response after {} iterations",
                        iteration_count
                    );
                } else {
                    // Only set full_response if it's empty (first iteration without tools)
                    // This prevents duplication when the agent responds without calling final_output
                    if full_response.is_empty() && !current_response.is_empty() {
                        full_response = current_response.clone();
                        debug!(
                            "Set full_response from current_response: {} chars",
                            full_response.len()
                        );
                    }
                    self.ui_writer.println("");
                }

                let _ttft = first_token_time.unwrap_or_else(|| stream_start.elapsed());

                // Add timing if needed
                let final_response = if show_timing {
                    format!(
                        "{}\n\n⏱️ {} | 💭 {}",
                        full_response,
                        Self::format_duration(stream_start.elapsed()),
                        Self::format_duration(_ttft)
                    )
                } else {
                    full_response
                };

                return Ok(TaskResult::new(final_response, self.context_window.clone()));
            }

            // Continue the loop to start a new stream with updated context
        }

        // If we exit the loop due to max iterations
        let _ttft = first_token_time.unwrap_or_else(|| stream_start.elapsed());

        // Add timing if needed
        let final_response = if show_timing {
            format!(
                "{}\n\n⏱️ {} | 💭 {}",
                full_response,
                Self::format_duration(stream_start.elapsed()),
                Self::format_duration(_ttft)
            )
        } else {
            full_response
        };

        Ok(TaskResult::new(final_response, self.context_window.clone()))
    }

    async fn execute_tool(&self, tool_call: &ToolCall) -> Result<String> {
        debug!("=== EXECUTING TOOL ===");
        debug!("Tool name: {}", tool_call.tool);
        debug!("Tool args (raw): {:?}", tool_call.args);
        debug!(
            "Tool args (JSON): {}",
            serde_json::to_string(&tool_call.args)
                .unwrap_or_else(|_| "failed to serialize".to_string())
        );
        debug!("======================");

        match tool_call.tool.as_str() {
            "shell" => {
                debug!("Processing shell tool call");
                if let Some(command) = tool_call.args.get("command") {
                    debug!("Found command parameter: {:?}", command);
                    if let Some(command_str) = command.as_str() {
                        debug!("Command string: {}", command_str);
                        // Use shell escaping to handle filenames with spaces and special characters
                        let escaped_command = shell_escape_command(command_str);

                        let executor = CodeExecutor::new();

                        // Create a receiver for streaming output
                        struct ToolOutputReceiver<'a, W: UiWriter> {
                            ui_writer: &'a W,
                        }

                        impl<'a, W: UiWriter> g3_execution::OutputReceiver for ToolOutputReceiver<'a, W> {
                            fn on_output_line(&self, line: &str) {
                                self.ui_writer.update_tool_output_line(line);
                            }
                        }

                        let receiver = ToolOutputReceiver {
                            ui_writer: &self.ui_writer,
                        };

                        match executor
                            .execute_bash_streaming(&escaped_command, &receiver)
                            .await
                        {
                            Ok(result) => {
                                if result.success {
                                    Ok(if result.stdout.is_empty() {
                                        "✅ Command executed successfully".to_string()
                                    } else {
                                        result.stdout.trim().to_string()
                                    })
                                } else {
                                    Ok(format!("❌ Command failed: {}", result.stderr.trim()))
                                }
                            }
                            Err(e) => Ok(format!("❌ Execution error: {}", e)),
                        }
                    } else {
                        debug!("Command parameter is not a string: {:?}", command);
                        Ok("❌ Invalid command argument".to_string())
                    }
                } else {
                    debug!("No command parameter found in args: {:?}", tool_call.args);
                    debug!(
                        "Available keys: {:?}",
                        tool_call
                            .args
                            .as_object()
                            .map(|obj| obj.keys().collect::<Vec<_>>())
                    );
                    Ok("❌ Missing command argument".to_string())
                }
            }
            "read_file" => {
                debug!("Processing read_file tool call");
                if let Some(file_path) = tool_call.args.get("file_path") {
                    if let Some(path_str) = file_path.as_str() {
                        // Expand tilde (~) to home directory
                        let expanded_path = shellexpand::tilde(path_str);
                        let path_str = expanded_path.as_ref();

                        // Check if this is an image file
                        let is_image = path_str.to_lowercase().ends_with(".png")
                            || path_str.to_lowercase().ends_with(".jpg")
                            || path_str.to_lowercase().ends_with(".jpeg")
                            || path_str.to_lowercase().ends_with(".gif")
                            || path_str.to_lowercase().ends_with(".bmp")
                            || path_str.to_lowercase().ends_with(".tiff")
                            || path_str.to_lowercase().ends_with(".tif")
                            || path_str.to_lowercase().ends_with(".webp");

                        // If it's an image file, use OCR via extract_text
                        if is_image {
                            if let Some(controller) = &self.computer_controller {
                                match controller.extract_text_from_image(path_str).await {
                                    Ok(result) => {
                                        return Ok(format!(
                                            "📄 Image file (OCR extracted, confidence: {:.2}):\n{}",
                                            result.confidence, result.text
                                        ));
                                    }
                                    Err(e) => {
                                        return Ok(format!(
                                            "❌ Failed to extract text from image '{}': {}",
                                            path_str, e
                                        ))
                                    }
                                }
                            } else {
                                return Ok("❌ Computer control not enabled. Cannot perform OCR on image files. Set computer_control.enabled = true in config.".to_string());
                            }
                        }

                        // Extract optional start and end positions
                        let start_char = tool_call
                            .args
                            .get("start")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize);
                        let end_char = tool_call
                            .args
                            .get("end")
                            .and_then(|v| v.as_u64())
                            .map(|n| n as usize);

                        debug!(
                            "Reading file: {}, start={:?}, end={:?}",
                            path_str, start_char, end_char
                        );

                        match std::fs::read_to_string(path_str) {
                            Ok(content) => {
                                // Validate and apply range if specified
                                let start = start_char.unwrap_or(0);
                                let end = end_char.unwrap_or(content.len());

                                // Validation
                                if start > content.len() {
                                    return Ok(format!(
                                        "❌ Start position {} exceeds file length {}",
                                        start,
                                        content.len()
                                    ));
                                }
                                if end > content.len() {
                                    return Ok(format!(
                                        "❌ End position {} exceeds file length {}",
                                        end,
                                        content.len()
                                    ));
                                }
                                if start > end {
                                    return Ok(format!(
                                        "❌ Start position {} is greater than end position {}",
                                        start, end
                                    ));
                                }

                                // Extract the requested portion, ensuring we're at char boundaries
                                // Find the nearest valid char boundaries
                                let start_boundary = if start == 0 {
                                    0
                                } else {
                                    content
                                        .char_indices()
                                        .find(|(i, _)| *i >= start)
                                        .map(|(i, _)| i)
                                        .unwrap_or(start)
                                };
                                let end_boundary = content
                                    .char_indices()
                                    .find(|(i, _)| *i >= end)
                                    .map(|(i, _)| i)
                                    .unwrap_or(content.len());

                                let partial_content = &content[start_boundary..end_boundary];
                                let line_count = partial_content.lines().count();
                                let total_lines = content.lines().count();

                                // Format output with range info if partial
                                if start_char.is_some() || end_char.is_some() {
                                    Ok(format!(
                                        "📄 File content (chars {}-{}, {} lines of {} total):\n{}",
                                        start_boundary,
                                        end_boundary,
                                        line_count,
                                        total_lines,
                                        partial_content
                                    ))
                                } else {
                                    Ok(format!(
                                        "📄 File content ({} lines):\n{}",
                                        line_count, content
                                    ))
                                }
                            }
                            Err(e) => Ok(format!("❌ Failed to read file '{}': {}", path_str, e)),
                        }
                    } else {
                        Ok("❌ Invalid file_path argument".to_string())
                    }
                } else {
                    Ok("❌ Missing file_path argument".to_string())
                }
            }
            "write_file" => {
                debug!("Processing write_file tool call");
                debug!("Raw tool_call.args: {:?}", tool_call.args);
                debug!(
                    "Args as JSON: {}",
                    serde_json::to_string(&tool_call.args)
                        .unwrap_or_else(|_| "failed to serialize".to_string())
                );
                debug!(
                    "Args type: {:?}",
                    std::any::type_name_of_val(&tool_call.args)
                );
                debug!("Args is_object: {}", tool_call.args.is_object());
                debug!("Args is_array: {}", tool_call.args.is_array());
                debug!("Args is_null: {}", tool_call.args.is_null());

                // Try multiple argument formats that different providers might use
                let (path_str, content_str) = if let Some(args_obj) = tool_call.args.as_object() {
                    debug!(
                        "Args object keys: {:?}",
                        args_obj.keys().collect::<Vec<_>>()
                    );

                    // Format 1: Standard format with file_path and content
                    if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("file_path"), args_obj.get("content"))
                    {
                        debug!("Found file_path and content keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted file_path='{}', content_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("file_path or content values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 2: Anthropic-style with path and content
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("path"), args_obj.get("content"))
                    {
                        debug!("Found path and content keys (Anthropic style)");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted path='{}', content_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("path or content values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 3: Alternative naming with filename and text
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("filename"), args_obj.get("text"))
                    {
                        debug!("Found filename and text keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted filename='{}', text_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("filename or text values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    }
                    // Format 4: Alternative naming with file and data
                    else if let (Some(path_val), Some(content_val)) =
                        (args_obj.get("file"), args_obj.get("data"))
                    {
                        debug!("Found file and data keys");
                        if let (Some(path), Some(content)) =
                            (path_val.as_str(), content_val.as_str())
                        {
                            debug!(
                                "Successfully extracted file='{}', data_len={}",
                                path,
                                content.len()
                            );
                            (Some(path), Some(content))
                        } else {
                            debug!("file or data values are not strings: path_val={:?}, content_val={:?}", path_val, content_val);
                            (None, None)
                        }
                    } else {
                        debug!(
                            "No matching key patterns found. Available argument keys: {:?}",
                            args_obj.keys().collect::<Vec<_>>()
                        );
                        (None, None)
                    }
                } else {
                    debug!("Args is not an object, checking if it's an array");
                    // Format 5: Args might be an array [path, content]
                    if let Some(args_array) = tool_call.args.as_array() {
                        debug!("Args is an array with {} elements", args_array.len());
                        if args_array.len() >= 2 {
                            if let (Some(path), Some(content)) =
                                (args_array[0].as_str(), args_array[1].as_str())
                            {
                                debug!(
                                    "Successfully extracted from array: path='{}', content_len={}",
                                    path,
                                    content.len()
                                );
                                (Some(path), Some(content))
                            } else {
                                debug!(
                                    "Array elements are not strings: [0]={:?}, [1]={:?}",
                                    args_array[0], args_array[1]
                                );
                                (None, None)
                            }
                        } else {
                            debug!("Array has insufficient elements: {}", args_array.len());
                            (None, None)
                        }
                    } else {
                        debug!("Args is neither object nor array");
                        (None, None)
                    }
                };

                debug!(
                    "Final extracted values: path_str={:?}, content_str_len={:?}",
                    path_str,
                    content_str.map(|c| c.len())
                );

                if let (Some(path), Some(content)) = (path_str, content_str) {
                    // Expand tilde (~) to home directory
                    let expanded_path = shellexpand::tilde(path);
                    let path = expanded_path.as_ref();

                    debug!("Writing to file: {}", path);

                    // Create parent directories if they don't exist
                    if let Some(parent) = std::path::Path::new(path).parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return Ok(format!(
                                "❌ Failed to create parent directories for '{}': {}",
                                path, e
                            ));
                        }
                    }

                    match std::fs::write(path, content) {
                        Ok(()) => {
                            let line_count = content.lines().count();
                            let char_count = content.len();
                            Ok(format!(
                                "✅ Successfully wrote {} lines ({} characters)",
                                line_count, char_count
                            ))
                        }
                        Err(e) => Ok(format!("❌ Failed to write to file '{}': {}", path, e)),
                    }
                } else {
                    // Provide more detailed error information
                    let available_keys = if let Some(obj) = tool_call.args.as_object() {
                        obj.keys().collect::<Vec<_>>()
                    } else {
                        vec![]
                    };

                    Ok(format!(
                        "❌ Missing file_path or content argument. Available keys: {:?}. Expected formats: {{\"file_path\": \"...\", \"content\": \"...\"}}, {{\"path\": \"...\", \"content\": \"...\"}}, {{\"filename\": \"...\", \"text\": \"...\"}}, or {{\"file\": \"...\", \"data\": \"...\"}}",
                        available_keys
                    ))
                }
            }
            "str_replace" => {
                debug!("Processing str_replace tool call");

                // Extract arguments
                let args_obj = match tool_call.args.as_object() {
                    Some(obj) => obj,
                    None => return Ok("❌ Invalid arguments: expected object".to_string()),
                };

                let file_path = match args_obj.get("file_path").and_then(|v| v.as_str()) {
                    Some(path) => {
                        // Expand tilde (~) to home directory
                        let expanded_path = shellexpand::tilde(path);
                        expanded_path.into_owned()
                    }
                    None => return Ok("❌ Missing or invalid file_path argument".to_string()),
                };

                let diff = match args_obj.get("diff").and_then(|v| v.as_str()) {
                    Some(d) => d,
                    None => return Ok("❌ Missing or invalid diff argument".to_string()),
                };

                // Optional start and end character positions (0-indexed, end is EXCLUSIVE)
                let start_char = args_obj
                    .get("start")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let end_char = args_obj
                    .get("end")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                debug!(
                    "str_replace: path={}, start={:?}, end={:?}",
                    file_path, start_char, end_char
                );

                // Read the existing file
                let file_content = match std::fs::read_to_string(&file_path) {
                    Ok(content) => content,
                    Err(e) => return Ok(format!("❌ Failed to read file '{}': {}", file_path, e)),
                };

                // Apply unified diff to content
                let result =
                    match apply_unified_diff_to_string(&file_content, diff, start_char, end_char) {
                        Ok(r) => r,
                        Err(e) => return Ok(format!("❌ {}", e)),
                    };

                // Write the result back to the file
                match std::fs::write(&file_path, &result) {
                    Ok(()) => Ok(format!("✅ applied unified diff")),
                    Err(e) => Ok(format!("❌ Failed to write to file '{}': {}", file_path, e)),
                }
            }
            "final_output" => {
                if let Some(summary) = tool_call.args.get("summary") {
                    if let Some(summary_str) = summary.as_str() {
                        Ok(format!("{}", summary_str))
                    } else {
                        Ok("✅ Turn completed".to_string())
                    }
                } else {
                    Ok("✅ Turn completed".to_string())
                }
            }
            "mouse_click" => {
                if let Some(controller) = &self.computer_controller {
                    let x = tool_call
                        .args
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    let y = tool_call
                        .args
                        .get("y")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    let button_str = tool_call
                        .args
                        .get("button")
                        .and_then(|v| v.as_str())
                        .unwrap_or("left");

                    let button = match button_str {
                        "left" => g3_computer_control::types::MouseButton::Left,
                        "right" => g3_computer_control::types::MouseButton::Right,
                        "middle" => g3_computer_control::types::MouseButton::Middle,
                        _ => g3_computer_control::types::MouseButton::Left,
                    };

                    match controller.move_mouse(x, y).await {
                        Ok(_) => {
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            match controller.click(button).await {
                                Ok(_) => Ok(format!(
                                    "✅ Clicked {} button at ({}, {})",
                                    button_str, x, y
                                )),
                                Err(e) => Ok(format!("❌ Failed to click: {}", e)),
                            }
                        }
                        Err(e) => Ok(format!("❌ Failed to move mouse: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "type_text" => {
                if let Some(controller) = &self.computer_controller {
                    let text = tool_call
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text argument"))?;

                    match controller.type_text(text).await {
                        Ok(_) => Ok(format!("✅ Typed text: {}", text)),
                        Err(e) => Ok(format!("❌ Failed to type text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "find_element" => {
                if let Some(controller) = &self.computer_controller {
                    let selector = g3_computer_control::types::ElementSelector {
                        text: tool_call
                            .args
                            .get("text")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        role: tool_call
                            .args
                            .get("role")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        window_id: tool_call
                            .args
                            .get("window_id")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    };

                    match controller.find_element(&selector).await {
                        Ok(Some(element)) => match serde_json::to_string_pretty(&element) {
                            Ok(json) => Ok(format!("✅ Found element:\n{}", json)),
                            Err(e) => {
                                Ok(format!("✅ Found element but failed to serialize: {}", e))
                            }
                        },
                        Ok(None) => Ok("❌ Element not found".to_string()),
                        Err(e) => Ok(format!("❌ Failed to find element: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "take_screenshot" => {
                if let Some(controller) = &self.computer_controller {
                    let path = tool_call
                        .args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing path argument"))?;

                    // Extract window_id (app name) if provided
                    let window_id = tool_call.args.get("window_id").and_then(|v| v.as_str());

                    // Extract region if provided
                    let region = tool_call
                        .args
                        .get("region")
                        .and_then(|v| v.as_object())
                        .map(|region_obj| g3_computer_control::types::Rect {
                            x: region_obj.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            y: region_obj.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            width: region_obj
                                .get("width")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                            height: region_obj
                                .get("height")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                        });

                    match controller.take_screenshot(path, region, window_id).await {
                        Ok(_) => {
                            // Get the actual path where the screenshot was saved
                            let actual_path = if path.starts_with('/') {
                                path.to_string()
                            } else {
                                let temp_dir = std::env::var("TMPDIR")
                                    .or_else(|_| {
                                        std::env::var("HOME").map(|h| format!("{}/tmp", h))
                                    })
                                    .unwrap_or_else(|_| "/tmp".to_string());
                                format!("{}/{}", temp_dir.trim_end_matches('/'), path)
                            };

                            if let Some(app) = window_id {
                                Ok(format!(
                                    "✅ Screenshot of {} saved to: {}",
                                    app, actual_path
                                ))
                            } else {
                                Ok(format!("✅ Screenshot saved to: {}", actual_path))
                            }
                        }
                        Err(e) => Ok(format!("❌ Failed to take screenshot: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "extract_text" => {
                if let Some(controller) = &self.computer_controller {
                    // Check if we have a path or a region
                    if let Some(path) = tool_call.args.get("path").and_then(|v| v.as_str()) {
                        // Extract text from image file
                        match controller.extract_text_from_image(path).await {
                            Ok(result) => Ok(format!(
                                "✅ Extracted text (confidence: {:.2}):\n{}",
                                result.confidence, result.text
                            )),
                            Err(e) => Ok(format!("❌ Failed to extract text: {}", e)),
                        }
                    } else if let Some(region_obj) =
                        tool_call.args.get("region").and_then(|v| v.as_object())
                    {
                        // Extract text from screen region
                        let region = g3_computer_control::types::Rect {
                            x: region_obj.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            y: region_obj.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                            width: region_obj
                                .get("width")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                            height: region_obj
                                .get("height")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0) as i32,
                        };

                        match controller.extract_text_from_screen(region).await {
                            Ok(result) => Ok(format!(
                                "✅ Extracted text (confidence: {:.2}):\n{}",
                                result.confidence, result.text
                            )),
                            Err(e) => Ok(format!("❌ Failed to extract text: {}", e)),
                        }
                    } else {
                        Ok("❌ Missing path or region argument".to_string())
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "find_text_on_screen" => {
                if let Some(controller) = &self.computer_controller {
                    let text = tool_call
                        .args
                        .get("text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("Missing text argument"))?;

                    match controller.find_text_on_screen(text).await {
                        Ok(Some(point)) => Ok(format!(
                            "✅ Found text '{}' at coordinates ({}, {})",
                            text, point.x, point.y
                        )),
                        Ok(None) => Ok(format!("❌ Text '{}' not found on screen", text)),
                        Err(e) => Ok(format!("❌ Failed to search for text: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "list_windows" => {
                if let Some(controller) = &self.computer_controller {
                    match controller.list_windows().await {
                        Ok(windows) => {
                            if windows.is_empty() {
                                Ok("📋 No windows found".to_string())
                            } else {
                                let mut output = format!("📋 Found {} windows:\n\n", windows.len());
                                for window in windows {
                                    output.push_str(&format!(
                                        "• **{}** ({}x{})\n  ID: `{}`\n  Title: {}\n\n",
                                        window.app_name,
                                        window.bounds.width,
                                        window.bounds.height,
                                        window.id,
                                        if window.title.is_empty() {
                                            "(no title)"
                                        } else {
                                            &window.title
                                        }
                                    ));
                                }
                                Ok(output)
                            }
                        }
                        Err(e) => Ok(format!("❌ Failed to list windows: {}", e)),
                    }
                } else {
                    Ok("❌ Computer control not enabled. Set computer_control.enabled = true in config.".to_string())
                }
            }
            "todo_read" => {
                debug!("Processing todo_read tool call");
                let content = self.todo_content.read().await;
                if content.is_empty() {
                    Ok("📝 TODO list is empty".to_string())
                } else {
                    Ok(format!("📝 TODO list:\n{}", content.as_str()))
                }
            }
            "todo_write" => {
                debug!("Processing todo_write tool call");
                if let Some(content) = tool_call.args.get("content") {
                    if let Some(content_str) = content.as_str() {
                        let char_count = content_str.chars().count();
                        let max_chars = std::env::var("G3_TODO_MAX_CHARS")
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(50_000);

                        if max_chars > 0 && char_count > max_chars {
                            return Ok(format!("❌ TODO list too large: {} chars (max: {})", char_count, max_chars));
                        }

                        let mut todo = self.todo_content.write().await;
                        *todo = content_str.to_string();
                        Ok(format!("✅ TODO list updated ({} chars)", char_count))
                    } else {
                        Ok("❌ Invalid content argument".to_string())
                    }
                } else {
                    Ok("❌ Missing content argument".to_string())
                }
            }
            _ => {
                warn!("Unknown tool: {}", tool_call.tool);
                Ok(format!("❓ Unknown tool: {}", tool_call.tool))
            }
        }
    }

    fn format_duration(duration: Duration) -> String {
        let total_ms = duration.as_millis();

        if total_ms < 1000 {
            format!("{}ms", total_ms)
        } else if total_ms < 60_000 {
            let seconds = duration.as_secs_f64();
            format!("{:.1}s", seconds)
        } else {
            let minutes = total_ms / 60_000;
            let remaining_seconds = (total_ms % 60_000) as f64 / 1000.0;
            format!("{}m {:.1}s", minutes, remaining_seconds)
        }
    }
}

// Note: JSON tool call filtering is now handled by fixed_filter_json::fixed_filter_json_tool_calls

// Apply unified diff to an input string with optional [start, end) bounds
pub fn apply_unified_diff_to_string(
    file_content: &str,
    diff: &str,
    start_char: Option<usize>,
    end_char: Option<usize>,
) -> Result<String> {
    // Parse full unified diff into hunks and apply sequentially.
    let hunks = parse_unified_diff_hunks(diff);
    if hunks.is_empty() {
        anyhow::bail!(
            "Invalid diff format. Expected unified diff with @@ hunks or +/- with context lines"
        );
    }

    // Normalize line endings to avoid CRLF/CR mismatches
    let content_norm = file_content.replace("\r\n", "\n").replace('\r', "\n");

    // Determine and validate the search range
    let search_start = start_char.unwrap_or(0);
    let search_end = end_char.unwrap_or(content_norm.len());

    if search_start > content_norm.len() {
        anyhow::bail!(
            "start position {} exceeds file length {}",
            search_start,
            content_norm.len()
        );
    }
    if search_end > content_norm.len() {
        anyhow::bail!(
            "end position {} exceeds file length {}",
            search_end,
            content_norm.len()
        );
    }
    if search_start > search_end {
        anyhow::bail!(
            "start position {} is greater than end position {}",
            search_start,
            search_end
        );
    }

    // Extract the region we're going to modify, ensuring we're at char boundaries
    // Find the nearest valid char boundaries
    let start_boundary = if search_start == 0 {
        0
    } else {
        content_norm
            .char_indices()
            .find(|(i, _)| *i >= search_start)
            .map(|(i, _)| i)
            .unwrap_or(search_start)
    };
    let end_boundary = content_norm
        .char_indices()
        .find(|(i, _)| *i >= search_end)
        .map(|(i, _)| i)
        .unwrap_or(content_norm.len());

    let mut region_content = content_norm[start_boundary..end_boundary].to_string();

    // Apply hunks in order
    for (idx, (old_block, new_block)) in hunks.iter().enumerate() {
        debug!(
            "Applying hunk {}: old_len={}, new_len={}",
            idx + 1,
            old_block.len(),
            new_block.len()
        );

        if let Some(pos) = region_content.find(old_block) {
            let endpos = pos + old_block.len();
            region_content.replace_range(pos..endpos, new_block);
        } else {
            // Not found; provide helpful diagnostics with a short preview
            let preview_len = old_block.len().min(200);
            let mut old_preview = old_block[..preview_len].to_string();
            if old_block.len() > preview_len {
                old_preview.push_str("...");
            }

            let range_note = if start_char.is_some() || end_char.is_some() {
                format!(
                    " (within character range {}:{})",
                    start_boundary, end_boundary
                )
            } else {
                String::new()
            };

            anyhow::bail!(
                "Pattern not found in file{}\nHunk {} failed. Searched for:\n{}",
                range_note,
                idx + 1,
                old_preview
            );
        }
    }

    // Reconstruct the full content with the modified region
    let mut result = String::with_capacity(content_norm.len() + region_content.len());
    result.push_str(&content_norm[..start_boundary]);
    result.push_str(&region_content);
    result.push_str(&content_norm[end_boundary..]);
    Ok(result)
}

// Parse a unified diff into a list of hunks as (old_block, new_block)
// Each hunk contains the exact text to search for and the replacement text including context lines.
fn parse_unified_diff_hunks(diff: &str) -> Vec<(String, String)> {
    let mut hunks: Vec<(String, String)> = Vec::new();

    let mut old_lines: Vec<String> = Vec::new();
    let mut new_lines: Vec<String> = Vec::new();
    let mut in_hunk = false;

    for raw_line in diff.lines() {
        let line = raw_line;

        // Skip common diff headers
        if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
        {
            continue;
        }

        if line.starts_with("--- ") || line.starts_with("+++ ") {
            // File header lines — ignore
            continue;
        }

        if line.starts_with("@@") {
            // Starting a new hunk — flush previous if present
            if in_hunk && (!old_lines.is_empty() || !new_lines.is_empty()) {
                hunks.push((old_lines.join("\n"), new_lines.join("\n")));
                old_lines.clear();
                new_lines.clear();
            }
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            // Some minimal diffs may omit @@; start collecting once we see diff markers
            if line.starts_with(' ')
                || (line.starts_with('-') && !line.starts_with("---"))
                || (line.starts_with('+') && !line.starts_with("+++"))
            {
                in_hunk = true;
            } else {
                continue;
            }
        }

        if line.starts_with(' ') {
            let content = &line[1..];
            old_lines.push(content.to_string());
            new_lines.push(content.to_string());
        } else if line.starts_with('+') && !line.starts_with("+++") {
            new_lines.push(line[1..].to_string());
        } else if line.starts_with('-') && !line.starts_with("---") {
            old_lines.push(line[1..].to_string());
        } else if line.starts_with('\\') {
            // Example: "\\ No newline at end of file" — ignore
            continue;
        } else {
            // Unknown line type — ignore
        }
    }

    if in_hunk && (!old_lines.is_empty() || !new_lines.is_empty()) {
        hunks.push((old_lines.join("\n"), new_lines.join("\n")));
    }

    hunks
}

// Helper function to properly escape shell commands
fn shell_escape_command(command: &str) -> String {
    // Simple approach: if the command contains file paths with spaces,
    // we need to be more intelligent about escaping

    // For now, let's use a basic approach that handles common cases
    // This is a simplified version - a full implementation would use proper shell parsing

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return command.to_string();
    }

    let cmd = parts[0];
    let _args = &parts[1..];

    // Commands that typically take file paths as arguments
    let file_commands = [
        "cat", "ls", "cp", "mv", "rm", "chmod", "chown", "file", "head", "tail", "wc", "grep",
    ];

    if file_commands.contains(&cmd) {
        // For file commands, we need to be smarter about escaping
        // Let's use a different approach: use the original command but wrap it in quotes if needed

        // Check if the command already has proper quoting
        if command.contains('"') || command.contains('\'') {
            // Already has some quoting, use as-is
            return command.to_string();
        }

        // Look for file paths that need escaping (contain spaces but aren't quoted)
        let mut escaped_command = String::new();
        let mut in_quotes = false;
        let mut current_word = String::new();
        let mut words = Vec::new();

        for ch in command.chars() {
            match ch {
                ' ' if !in_quotes => {
                    if !current_word.is_empty() {
                        words.push(current_word.clone());
                        current_word.clear();
                    }
                }
                '"' => {
                    in_quotes = !in_quotes;
                    current_word.push(ch);
                }
                _ => {
                    current_word.push(ch);
                }
            }
        }

        if !current_word.is_empty() {
            words.push(current_word);
        }

        // Reconstruct the command with proper escaping
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                escaped_command.push(' ');
            }

            // If this word looks like a file path (contains / or ~) and has spaces, quote it
            if word.contains('/') || word.starts_with('~') {
                if word.contains(' ') && !word.starts_with('"') && !word.starts_with('\'') {
                    escaped_command.push_str(&format!("\"{}\"", word));
                } else {
                    escaped_command.push_str(word);
                }
            } else {
                escaped_command.push_str(word);
            }
        }

        escaped_command
    } else {
        // For non-file commands, use the original command
        command.to_string()
    }
}

// Helper function to fix mixed quotes in JSON strings
#[allow(dead_code)]
fn fix_nested_quotes_in_shell_command(json_str: &str) -> String {
    let mut _result = String::new();
    let _chars = json_str.chars().peekable();
    // Example: {"tool": "shell", "args": {"command": "python -c 'import os; print("hello")'"}

    // Look for the pattern: "command": "
    if let Some(command_start) = json_str.find(r#""command": ""#) {
        let command_value_start = command_start + r#""command": ""#.len();

        // Find the end of the command string by looking for the pattern "}
        // We need to be careful about nested quotes
        if let Some(end_marker) = json_str[command_value_start..].find(r#"" }"#) {
            let command_end = command_value_start + end_marker;

            let before = &json_str[..command_value_start];
            let command_content = &json_str[command_value_start..command_end];
            let after = &json_str[command_end..];

            // Fix the command content by properly escaping quotes
            let mut fixed_command = String::new();
            let mut chars = command_content.chars().peekable();

            while let Some(ch) = chars.next() {
                match ch {
                    '"' => {
                        // Check if this quote is already escaped
                        if fixed_command.ends_with('\\') {
                            fixed_command.push(ch); // Already escaped, keep as-is
                        } else {
                            fixed_command.push_str(r#"\""#); // Escape the quote
                        }
                    }
                    '\\' => {
                        // Check what follows the backslash
                        if let Some(&_next_ch) = chars.peek() {
                            if _next_ch == '"' {
                                // This is an escaped quote, keep the backslash
                                fixed_command.push(ch);
                            } else {
                                // Regular backslash, escape it
                                fixed_command.push_str(r#"\\"#);
                            }
                        } else {
                            // Backslash at end, escape it
                            fixed_command.push_str(r#"\\"#);
                        }
                    }
                    _ => fixed_command.push(ch),
                }
            }

            return format!("{}{}{}", before, fixed_command, after);
        }
    }

    // Fallback: if we can't parse the structure, try some basic replacements
    json_str.to_string()
}

// Helper function to fix mixed quotes in JSON (single quotes where double quotes should be)
#[allow(dead_code)]
fn fix_mixed_quotes_in_json(json_str: &str) -> String {
    let mut result = String::new();
    let mut chars = json_str.chars().peekable();
    let mut in_string = false;
    let mut string_delimiter = '"';

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_string => {
                // Start of a double-quoted string
                in_string = true;
                string_delimiter = '"';
                result.push(ch);
            }
            '\'' if !in_string => {
                // Start of a single-quoted string - convert to double quotes
                in_string = true;
                string_delimiter = '\'';
                result.push('"'); // Convert single quote to double quote
            }
            c if in_string && c == string_delimiter => {
                // End of current string
                if string_delimiter == '\'' {
                    result.push('"'); // Convert single quote to double quote
                } else {
                    result.push(c);
                }
                in_string = false;
            }
            '"' if in_string && string_delimiter == '\'' => {
                // Double quote inside single-quoted string - escape it
                result.push_str(r#"\""#);
            }
            '\\' if in_string => {
                // Escape sequence - preserve it
                result.push(ch);
                if let Some(&_next_ch) = chars.peek() {
                    result.push(chars.next().unwrap());
                }
            }
            _ => {
                result.push(ch);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::parse_unified_diff_hunks;

    #[test]
    fn parses_minimal_unified_diff_without_hunk_header() {
        let diff = "--- old\n-old text\n+++ new\n+new text\n";
        let hunks = parse_unified_diff_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].0, "old text");
        assert_eq!(hunks[0].1, "new text");
    }

    #[test]
    fn parses_diff_with_context_and_hunk_headers() {
        let diff = "@@ -1,3 +1,3 @@\n common\n-old\n+new\n common2\n";
        let hunks = parse_unified_diff_hunks(diff);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].0, "common\nold\ncommon2");
        assert_eq!(hunks[0].1, "common\nnew\ncommon2");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::apply_unified_diff_to_string;

    #[test]
    fn apply_multi_hunk_unified_diff_to_string() {
        let original = "line 1\nkeep\nold A\nkeep 2\nold B\nkeep 3\n";
        let diff =
            "@@ -1,6 +1,6 @@\n line 1\n keep\n-old A\n+new A\n keep 2\n-old B\n+new B\n keep 3\n";
        let result = apply_unified_diff_to_string(original, diff, None, None).unwrap();
        let expected = "line 1\nkeep\nnew A\nkeep 2\nnew B\nkeep 3\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn apply_diff_within_range_only() {
        let original = "A\nold\nB\nold\nC\n";
        // Only the first 'old' should be replaced due to range
        let diff = "@@ -1,3 +1,3 @@\n A\n-old\n+NEW\n B\n";
        let start = 0usize; // Start of file
        let end = original.find("B\n").unwrap() + 2; // up to end of line 'B\n'
        let result = apply_unified_diff_to_string(original, diff, Some(start), Some(end)).unwrap();
        let expected = "A\nNEW\nB\nold\nC\n";
        assert_eq!(result, expected);
    }
}
