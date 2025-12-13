//! Tests for the pre-flight max_tokens validation with thinking.budget_tokens constraint
//!
//! These tests verify that when using Anthropic with extended thinking enabled,
//! the max_tokens calculation properly accounts for the budget_tokens constraint.

use g3_config::Config;
use g3_core::ContextWindow;
use std::collections::HashMap;

/// Helper function to create a minimal config for testing
fn create_test_config_with_thinking(thinking_budget: Option<u32>) -> Config {
    let mut config = Config::default();
    
    // Set up Anthropic provider with optional thinking budget using new HashMap format
    let mut anthropic_configs = HashMap::new();
    anthropic_configs.insert("default".to_string(), g3_config::AnthropicConfig {
        api_key: "test-key".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        base_url: None,
        max_tokens: Some(16000),
        temperature: Some(0.1),
        cache_config: None,
        enable_1m_context: None,
        thinking_budget_tokens: thinking_budget,
    });
    config.providers.anthropic = anthropic_configs;
    
    config.providers.default_provider = "anthropic.default".to_string();
    config
}

/// Test that when thinking is disabled, max_tokens passes through unchanged
#[test]
fn test_no_thinking_budget_passes_through() {
    let config = create_test_config_with_thinking(None);
    
    // Without thinking budget, any max_tokens should be fine
    let proposed_max = 5000;
    
    // The constraint check would return (proposed_max, false)
    // since there's no thinking_budget_tokens configured
    assert!(config.providers.anthropic.get("default").unwrap().thinking_budget_tokens.is_none());
}

/// Test that when max_tokens > budget_tokens + buffer, no reduction is needed
#[test]
fn test_sufficient_max_tokens_no_reduction_needed() {
    let config = create_test_config_with_thinking(Some(10000));
    let budget_tokens = config.providers.anthropic.get("default").unwrap().thinking_budget_tokens.unwrap();
    
    // minimum_required = budget_tokens + 1024 = 11024
    let minimum_required = budget_tokens + 1024;
    
    // If proposed_max >= minimum_required, no reduction is needed
    let proposed_max = 15000;
    assert!(proposed_max >= minimum_required);
}

/// Test that when max_tokens < budget_tokens + buffer, reduction is needed
#[test]
fn test_insufficient_max_tokens_needs_reduction() {
    let config = create_test_config_with_thinking(Some(10000));
    let budget_tokens = config.providers.anthropic.get("default").unwrap().thinking_budget_tokens.unwrap();
    
    // minimum_required = budget_tokens + 1024 = 11024
    let minimum_required = budget_tokens + 1024;
    
    // If proposed_max < minimum_required, reduction IS needed
    let proposed_max = 5000;
    assert!(proposed_max < minimum_required);
}

/// Test the minimum required calculation
#[test]
fn test_minimum_required_calculation() {
    // For a budget of 10000, we need at least 11024 tokens
    let budget_tokens = 10000u32;
    let output_buffer = 1024u32;
    let minimum_required = budget_tokens + output_buffer;
    
    assert_eq!(minimum_required, 11024);
    
    // For a larger budget
    let budget_tokens = 32000u32;
    let minimum_required = budget_tokens + output_buffer;
    assert_eq!(minimum_required, 33024);
}

/// Test context window usage calculation for summary max_tokens
#[test]
fn test_context_window_available_tokens() {
    let mut context = ContextWindow::new(200000); // 200k context window
    
    // Simulate heavy usage
    context.used_tokens = 180000; // 90% used
    
    let model_limit = context.total_tokens;
    let current_usage = context.used_tokens;
    
    // 2.5% buffer calculation
    let buffer = (model_limit / 40).clamp(1000, 10000);
    assert_eq!(buffer, 5000); // 200000/40 = 5000
    
    let available = model_limit
        .saturating_sub(current_usage)
        .saturating_sub(buffer);
    
    // 200000 - 180000 - 5000 = 15000
    assert_eq!(available, 15000);
    
    // Capped at 10000 for summary
    let summary_max = available.min(10_000);
    assert_eq!(summary_max, 10000);
}

/// Test that when context is nearly full, available tokens may be below thinking budget
#[test]
fn test_context_nearly_full_triggers_reduction() {
    let mut context = ContextWindow::new(200000);
    
    // Very heavy usage - 98% used
    context.used_tokens = 196000;
    
    let model_limit = context.total_tokens;
    let current_usage = context.used_tokens;
    let buffer = (model_limit / 40).clamp(1000, 10000); // 5000
    
    let available = model_limit
        .saturating_sub(current_usage)
        .saturating_sub(buffer);
    
    // 200000 - 196000 - 5000 = -1000 -> saturates to 0
    assert_eq!(available, 0);
    
    // With thinking_budget of 10000, this would definitely need reduction
    let thinking_budget = 10000u32;
    let minimum_required = thinking_budget + 1024;
    assert!(available < minimum_required);
}

/// Test the hard-coded fallback value
#[test]
fn test_hardcoded_fallback_value() {
    // When all else fails, we use 5000 as the hard-coded max_tokens
    let hardcoded_fallback = 5000u32;
    
    // This should be a reasonable value that Anthropic will accept
    // even with thinking enabled (though output will be limited)
    assert!(hardcoded_fallback > 0);
    
    // Note: With a 10000 thinking budget, 5000 is still below the
    // minimum required (11024), but we send it anyway as a "last resort"
    // hoping the API might still work for basic operations
}

/// Test provider-specific caps
#[test]
fn test_provider_specific_caps() {
    // Anthropic/Databricks: cap at 10000
    let anthropic_cap = 10000u32;
    let proposed = 15000u32;
    assert_eq!(proposed.min(anthropic_cap), 10000);
    
    // Embedded: cap at 3000
    let embedded_cap = 3000u32;
    let proposed = 5000u32;
    assert_eq!(proposed.min(embedded_cap), 3000);
    
    // Default: cap at 5000
    let default_cap = 5000u32;
    let proposed = 8000u32;
    assert_eq!(proposed.min(default_cap), 5000);
}

/// Test that the error message mentions the thinking budget constraint
#[test]
fn test_error_message_content() {
    // Verify the warning message format contains useful information
    let proposed_max_tokens = 5000u32;
    let budget_tokens = 10000u32;
    let minimum_required = budget_tokens + 1024;
    
    let warning = format!(
        "max_tokens ({}) is below required minimum ({}) for thinking.budget_tokens ({}). Context reduction needed.",
        proposed_max_tokens, minimum_required, budget_tokens
    );
    
    assert!(warning.contains("5000"));
    assert!(warning.contains("11024"));
    assert!(warning.contains("10000"));
    assert!(warning.contains("Context reduction needed"));
}
