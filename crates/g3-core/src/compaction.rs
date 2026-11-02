//! Context compaction logic.
//!
//! This module provides unified compaction functionality used by both
//! manual compaction (`/compact` command) and automatic compaction
//! (when context window reaches capacity during streaming).

use anyhow::Result;
use g3_providers::{CompletionRequest, Message, MessageRole, ProviderRegistry};
use tracing::{debug, error, warn};

use crate::context_window::ContextWindow;
use crate::provider_config;
use crate::ui_writer::UiWriter;

/// Minimum tokens for summary requests to avoid API errors when context is nearly full.
pub const SUMMARY_MIN_TOKENS: u32 = 1000;

/// Result of a compaction operation.
#[derive(Debug)]
pub struct CompactionResult {
    /// Whether compaction succeeded
    pub success: bool,
    /// Characters saved by compaction (if successful)
    pub chars_saved: usize,
    /// Error message (if failed)
    pub error: Option<String>,
}

impl CompactionResult {
    pub fn success(chars_saved: usize) -> Self {
        Self {
            success: true,
            chars_saved,
            error: None,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            success: false,
            chars_saved: 0,
            error: Some(error),
        }
    }
}

/// Configuration for a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionConfig<'a> {
    /// Provider name (e.g., "anthropic", "openai")
    pub provider_name: &'a str,
    /// Latest user message to preserve after compaction
    pub latest_user_msg: Option<String>,
}

/// Calculate the summary max_tokens with provider-specific caps applied.
///
/// This is the canonical implementation - both manual and auto compaction use this.
pub fn calculate_capped_summary_tokens(
    config: &g3_config::Config,
    provider_name: &str,
    base_max_tokens: u32,
) -> u32 {
    // Apply provider-specific caps
    // For Anthropic with thinking enabled, we need max_tokens > thinking.budget_tokens
    // So we set a higher cap when thinking is configured
    let anthropic_cap = match provider_config::get_thinking_budget_tokens(config, provider_name) {
        Some(budget) => (budget + 2000).max(10_000), // At least budget + 2000 for response
        None => 10_000,
    };
    
    let capped = match provider_name {
        name if name.starts_with("anthropic") => base_max_tokens.min(anthropic_cap),
        _ => base_max_tokens.min(5000),
    };
    
    // Ensure minimum floor as defense-in-depth
    capped.max(SUMMARY_MIN_TOKENS)
}

/// Check if thinking mode should be disabled for a summary request.
///
/// Anthropic requires: max_tokens > thinking.budget_tokens + 1024
pub fn should_disable_thinking(
    config: &g3_config::Config,
    provider_name: &str,
    summary_max_tokens: u32,
) -> bool {
    provider_config::get_thinking_budget_tokens(config, provider_name).map_or(false, |budget| {
        let minimum_for_thinking = budget + 1024;
        let should_disable = summary_max_tokens <= minimum_for_thinking;
        if should_disable {
            warn!(
                "Disabling thinking mode for summary: max_tokens ({}) <= minimum_for_thinking ({})",
                summary_max_tokens, minimum_for_thinking
            );
        }
        should_disable
    })
}

/// Build the summary request messages from conversation history.
pub fn build_summary_messages(context_window: &ContextWindow) -> Vec<Message> {
    let summary_prompt = context_window.create_summary_prompt();
    
    let conversation_text = context_window
        .conversation_history
        .iter()
        .map(|m| format!("{:?}: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    
    vec![
        Message::new(
            MessageRole::System,
            "You are a helpful assistant that creates concise summaries.".to_string(),
        ),
        Message::new(
            MessageRole::User,
            format!(
                "Based on this conversation history, {}\n\nConversation:\n{}",
                summary_prompt, conversation_text
            ),
        ),
    ]
}

/// Perform context compaction by summarizing conversation history.
///
/// This is the unified implementation used by both:
/// - `force_compact()` - manual compaction via `/compact` command
/// - `stream_completion_with_tools()` - automatic compaction when context is full
///
/// # Arguments
/// * `providers` - Provider registry to get the LLM provider
/// * `context_window` - Context window to compact
/// * `config` - Application config for provider settings
/// * `compaction_config` - Configuration for this compaction operation
/// * `ui_writer` - UI writer for status messages
///
/// # Returns
/// `CompactionResult` indicating success/failure and chars saved
pub async fn perform_compaction<W: UiWriter>(
    providers: &ProviderRegistry,
    context_window: &mut ContextWindow,
    config: &g3_config::Config,
    compaction_config: CompactionConfig<'_>,
    ui_writer: &W,
    thinning_events: &mut Vec<usize>,
) -> Result<CompactionResult> {
    let provider_name = compaction_config.provider_name;
    
    // Apply fallback sequence: thinnify -> skinnify -> hard-coded 5000
    let base_max_tokens = apply_summary_fallback_sequence(
        context_window,
        config,
        provider_name,
        ui_writer,
        thinning_events,
    );
    
    let summary_max_tokens = calculate_capped_summary_tokens(config, provider_name, base_max_tokens);
    
    debug!(
        "Requesting summary with max_tokens: {} (current usage: {} tokens)",
        summary_max_tokens, context_window.used_tokens
    );
    
    // Build summary request
    let summary_messages = build_summary_messages(context_window);
    let provider = providers.get(None)?;
    
    let disable_thinking = should_disable_thinking(config, provider.name(), summary_max_tokens);
    
    debug!(
        "Creating summary request: max_tokens={}, disable_thinking={}",
        summary_max_tokens, disable_thinking
    );
    
    let summary_request = CompletionRequest {
        messages: summary_messages,
        max_tokens: Some(summary_max_tokens),
        temperature: Some(provider_config::resolve_temperature(config, provider.name())),
        stream: false,
        tools: None,
        disable_thinking,
    };
    
    // Execute summary request
    match provider.complete(summary_request).await {
        Ok(summary_response) => {
            // Note: ACD dehydration now happens at the end of each turn in Agent::dehydrate_context()
            // Compaction just does lossy summarization of the existing stubs + summaries
            let chars_saved = context_window.reset_with_summary(
                summary_response.content,
                compaction_config.latest_user_msg,
            );
            Ok(CompactionResult::success(chars_saved))
        }
        Err(e) => {
            error!("Failed to create summary: {}", e);
            Ok(CompactionResult::failure(e.to_string()))
        }
    }
}

/// Apply the fallback sequence for summary requests to free up context space.
///
/// Sequence: thinnify -> skinnify -> hard-coded minimum
fn apply_summary_fallback_sequence<W: UiWriter>(
    context_window: &mut ContextWindow,
    config: &g3_config::Config,
    provider_name: &str,
    ui_writer: &W,
    thinning_events: &mut Vec<usize>,
) -> u32 {
    // Initial validation
    let (mut max_tokens, needs_reduction) = provider_config::calculate_summary_max_tokens(
        config,
        provider_name,
        context_window.total_tokens,
        context_window.used_tokens,
    );
    
    if !needs_reduction {
        return max_tokens;
    }
    
    ui_writer.print_context_status(
        "⚠️ Context window too full for thinking budget. Applying fallback sequence...\n",
    );
    
    // Step 1: Try thinnify (first third of context)
    ui_writer.print_context_status("🥒 Step 1: Trying thinnify...\n");
    let thin_result = context_window.thin_context(None);
    thinning_events.push(thin_result.chars_saved);
    ui_writer.print_thin_result(&thin_result);
    
    // Recalculate after thinnify
    let (new_max, still_needs_reduction) = provider_config::calculate_summary_max_tokens(
        config,
        provider_name,
        context_window.total_tokens,
        context_window.used_tokens,
    );
    max_tokens = new_max;
    if !still_needs_reduction {
        ui_writer.print_context_status("✅ Thinnify resolved capacity issue. Continuing...\n");
        return max_tokens;
    }
    
    // Step 2: Try skinnify (entire context)
    ui_writer.print_context_status("🦴 Step 2: Trying skinnify...\n");
    let skinny_result = context_window.thin_context_all(None);
    thinning_events.push(skinny_result.chars_saved);
    ui_writer.print_thin_result(&skinny_result);
    
    // Recalculate after skinnify
    let (final_max, final_needs_reduction) = provider_config::calculate_summary_max_tokens(
        config,
        provider_name,
        context_window.total_tokens,
        context_window.used_tokens,
    );
    if !final_needs_reduction {
        ui_writer.print_context_status("✅ Skinnify resolved capacity issue. Continuing...\n");
        return final_max;
    }
    
    // Step 3: Nothing worked, use hard-coded minimum
    const HARD_CODED_MINIMUM: u32 = 5000;
    ui_writer.print_context_status(&format!(
        "⚠️ Step 3: Context reduction insufficient. Using hard-coded max_tokens={} as last resort...\n",
        HARD_CODED_MINIMUM
    ));
    HARD_CODED_MINIMUM
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_calculate_capped_summary_tokens_anthropic() {
        let config = g3_config::Config::default();
        // Without thinking budget, should cap at 10_000
        let result = calculate_capped_summary_tokens(&config, "anthropic", 20_000);
        assert_eq!(result, 10_000);
    }
    
    #[test]
    fn test_calculate_capped_summary_tokens_minimum_floor() {
        let config = g3_config::Config::default();
        // Even with very low input, should return at least SUMMARY_MIN_TOKENS
        let result = calculate_capped_summary_tokens(&config, "gemini", 100);
        assert_eq!(result, SUMMARY_MIN_TOKENS);
    }
    
    #[test]
    fn test_compaction_result_success() {
        let result = CompactionResult::success(5000);
        assert!(result.success);
        assert_eq!(result.chars_saved, 5000);
        assert!(result.error.is_none());
    }
    
    #[test]
    fn test_compaction_result_failure() {
        let result = CompactionResult::failure("test error".to_string());
        assert!(!result.success);
        assert_eq!(result.chars_saved, 0);
        assert_eq!(result.error, Some("test error".to_string()));
    }
}
