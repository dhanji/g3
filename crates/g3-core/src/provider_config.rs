//! Provider configuration resolution.
//!
//! This module handles resolving provider-specific configuration values
//! like max_tokens, temperature, and thinking budget tokens from the
//! hierarchical config structure.

use g3_config::Config;
use tracing::warn;

/// Minimum tokens for summary requests to avoid API errors when context is nearly full.
pub const SUMMARY_MIN_TOKENS: u32 = 1000;

/// Parse a provider reference into (provider_type, config_name).
/// Format: "provider_type.config_name" (e.g., "anthropic.default")
/// Falls back to (provider_name, "default") for simple names.
///
/// A trailing [`FALLBACK_PROVIDER_SUFFIX`] is stripped first, so the overload
/// fallback provider `anthropic.default#fallback` resolves to the *same*
/// config entry as `anthropic.default`. That is the mechanism by which the
/// fallback inherits max_tokens, temperature, cache settings, the thinking
/// budget and the 1M-context beta — differing from the default in the model
/// string alone. Without this strip, every one of those lookups would miss the
/// HashMap and quietly return a hardcoded default, so a fallback turn would
/// lose prompt caching and (worse) be accounted against a 200k window while
/// the API still allowed 1M.
pub fn parse_provider_ref(provider_name: &str) -> (&str, &str) {
    let provider_name = provider_name
        .strip_suffix(g3_providers::FALLBACK_PROVIDER_SUFFIX)
        .unwrap_or(provider_name);
    let parts: Vec<&str> = provider_name.split('.').collect();
    if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        (provider_name, "default")
    }
}

/// Get the configured max_tokens for a provider from config.
pub fn get_max_tokens(config: &Config, provider_name: &str) -> Option<u32> {
    let (provider_type, config_name) = parse_provider_ref(provider_name);
    
    match provider_type {
        "anthropic" => config.providers.anthropic.get(config_name)?.max_tokens,
        "openai" => config.providers.openai.get(config_name)?.max_tokens,
        "databricks" => config.providers.databricks.get(config_name)?.max_tokens,
        "embedded" => config.providers.embedded.get(config_name)?.max_tokens,
        _ => None,
    }
}

/// Get the configured temperature for a provider from config.
pub fn get_temperature(config: &Config, provider_name: &str) -> Option<f32> {
    let (provider_type, config_name) = parse_provider_ref(provider_name);
    
    match provider_type {
        "anthropic" => config.providers.anthropic.get(config_name)?.temperature,
        "openai" => config.providers.openai.get(config_name)?.temperature,
        "databricks" => config.providers.databricks.get(config_name)?.temperature,
        "embedded" => config.providers.embedded.get(config_name)?.temperature,
        _ => None,
    }
}

/// Get the thinking budget tokens for Anthropic provider, if configured.
pub fn get_thinking_budget_tokens(config: &Config, provider_name: &str) -> Option<u32> {
    let (provider_type, config_name) = parse_provider_ref(provider_name);
    
    // Only Anthropic has thinking_budget_tokens
    if provider_type != "anthropic" {
        return None;
    }
    
    config.providers.anthropic
        .get(config_name)
        .and_then(|c| c.thinking_budget_tokens)
}

/// Whether the 1M-token context beta is enabled for an Anthropic provider.
///
/// Only Anthropic supports this (via the `context-1m-2025-08-07` beta header),
/// so any other provider type returns false. An unset flag or unknown config
/// name also returns false, so callers can treat this as a plain bool.
pub fn is_1m_context_enabled(config: &Config, provider_name: &str) -> bool {
    let (provider_type, config_name) = parse_provider_ref(provider_name);

    // Only Anthropic has the 1M context beta
    if provider_type != "anthropic" {
        return false;
    }

    config.providers.anthropic
        .get(config_name)
        .and_then(|c| c.enable_1m_context)
        .unwrap_or(false)
}

/// Resolve the max_tokens to use for a given provider, applying fallbacks.
pub fn resolve_max_tokens(config: &Config, provider_name: &str) -> u32 {
    let (provider_type, _) = parse_provider_ref(provider_name);
    
    // Use provider-specific defaults that match the provider implementations
    // These defaults should match what the providers use internally
    let provider_default = match provider_type {
        "anthropic" => 32000,   // Anthropic provider defaults to 32768, we use 32000
        "databricks" => 32000,  // Databricks is passthru to Anthropic, match its defaults
        "openai" => 32000,      // OpenAI models support large outputs
        "embedded" => 8192,     // Embedded provider: let provider's effective_max_tokens() handle it
        _ => 16000,             // Generic fallback
    };
    let base = get_max_tokens(config, provider_name).unwrap_or(provider_default);
    
    // For Anthropic with thinking enabled, ensure max_tokens is sufficient
    // Anthropic requires: max_tokens > thinking.budget_tokens
    if provider_type == "anthropic" {
        if let Some(budget) = get_thinking_budget_tokens(config, provider_name) {
            let minimum_for_thinking = budget + 1024;
            return base.max(minimum_for_thinking);
        }
    }
    
    base
}

/// Resolve the temperature to use for a given provider, applying fallbacks.
pub fn resolve_temperature(config: &Config, provider_name: &str) -> f32 {
    let (provider_type, _) = parse_provider_ref(provider_name);
    
    match provider_type {
        "databricks" => get_temperature(config, provider_name).unwrap_or(0.1),
        _ => get_temperature(config, provider_name).unwrap_or(0.1),
    }
}

/// Pre-flight check to validate and adjust max_tokens for the thinking.budget_tokens constraint.
/// Returns the adjusted max_tokens that satisfies: max_tokens > thinking.budget_tokens
/// Also returns whether we need to apply fallback actions (thinnify/skinnify).
///
/// Returns: (adjusted_max_tokens, needs_context_reduction)
pub fn preflight_validate_max_tokens(
    config: &Config,
    provider_name: &str,
    proposed_max_tokens: u32,
) -> (u32, bool) {
    let (provider_type, _) = parse_provider_ref(provider_name);
    
    // Only applies to Anthropic provider
    if provider_type != "anthropic" {
        return (proposed_max_tokens, false);
    }

    let budget_tokens = match get_thinking_budget_tokens(config, provider_name) {
        Some(budget) => budget,
        None => return (proposed_max_tokens, false), // No thinking enabled
    };

    // Anthropic requires: max_tokens > budget_tokens
    // We add a minimum output buffer of 1024 tokens for actual response content
    let minimum_required = budget_tokens + 1024;

    if proposed_max_tokens >= minimum_required {
        // We have enough headroom
        (proposed_max_tokens, false)
    } else {
        // max_tokens is too low - need to either adjust or reduce context
        warn!(
            "max_tokens ({}) is below required minimum ({}) for thinking.budget_tokens ({}). Context reduction needed.",
            proposed_max_tokens, minimum_required, budget_tokens
        );
        // Return the minimum required, but flag that we need context reduction
        (minimum_required, true)
    }
}

/// Calculate max_tokens for a summary request, ensuring it satisfies the thinking constraint.
/// Returns (max_tokens, whether_fallback_is_needed)
/// 
/// IMPORTANT: Always returns at least SUMMARY_MIN_TOKENS to avoid API errors
/// when context is nearly full (90%+).
pub fn calculate_summary_max_tokens(
    config: &Config,
    provider_name: &str,
    model_limit: u32,
    current_usage: u32,
) -> (u32, bool) {
    let (provider_type, _) = parse_provider_ref(provider_name);
    
    // Get the configured max_tokens for this provider
    let configured_max_tokens = resolve_max_tokens(config, provider_name);
    
    // Calculate available tokens with buffer
    let buffer = (model_limit / 40).clamp(1000, 10000); // 2.5% buffer
    let available = model_limit
        .saturating_sub(current_usage)
        .saturating_sub(buffer);
    // Ensure we have at least a minimum floor for summary requests
    // This prevents max_tokens=0 errors when context is 90%+ full
    let available = available.max(SUMMARY_MIN_TOKENS);
    
    // Use the smaller of available tokens (with floor) or configured max_tokens,
    // but ensure we don't go below thinking budget floor for Anthropic
    let proposed_max_tokens = available.min(configured_max_tokens);
    let proposed_max_tokens = if provider_type == "anthropic" {
        if let Some(budget) = get_thinking_budget_tokens(config, provider_name) {
            proposed_max_tokens.max(budget + 1024)
        } else {
            proposed_max_tokens
        }
    } else {
        proposed_max_tokens
    };

    // Validate against thinking budget constraint
    preflight_validate_max_tokens(config, provider_name, proposed_max_tokens)
}

/// Get the provider-specific cap for summary max_tokens.
pub fn get_summary_max_tokens_cap(config: &Config, provider_name: &str) -> u32 {
    let (provider_type, _) = parse_provider_ref(provider_name);
    
    // For Anthropic with thinking enabled, we need max_tokens > thinking.budget_tokens
    // So we set a higher cap when thinking is configured
    match provider_type {
        "anthropic" => {
            match get_thinking_budget_tokens(config, provider_name) {
                Some(budget) => (budget + 2000).max(10_000),
                None => 10_000,
            }
        }
        "databricks" => 10_000,
        "embedded" => 3000,
        _ => 5000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_provider_ref_with_dot() {
        let (ptype, name) = parse_provider_ref("anthropic.default");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "default");
    }

    #[test]
    fn test_parse_provider_ref_simple() {
        let (ptype, name) = parse_provider_ref("anthropic");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "default");
    }

    #[test]
    fn test_parse_provider_ref_with_custom_name() {
        let (ptype, name) = parse_provider_ref("openai.gpt4");
        assert_eq!(ptype, "openai");
        assert_eq!(name, "gpt4");
    }

    // ── is_1m_context_enabled ──────────────────────────────────────────
    //
    // These build a Config from Config::default() and inject a single named
    // Anthropic entry, so we exercise the real HashMap lookup path.

    /// Build a config with one Anthropic provider named `name`, with
    /// `enable_1m_context` set to `flag`.
    fn config_with_anthropic(name: &str, flag: Option<bool>) -> g3_config::Config {
        let mut config = g3_config::Config::default();
        config.providers.anthropic.insert(
            name.to_string(),
            g3_config::AnthropicConfig {
                api_key: "sk-test".to_string(),
                model: "claude-opus-5".to_string(),
                max_tokens: None,
                temperature: None,
                cache_config: None,
                enable_1m_context: flag,
                thinking_budget_tokens: None,
            },
        );
        config
    }

    #[test]
    fn test_1m_context_enabled_when_flag_true() {
        let config = config_with_anthropic("default", Some(true));
        assert!(is_1m_context_enabled(&config, "anthropic.default"));
    }

    #[test]
    fn test_1m_context_disabled_when_flag_false() {
        let config = config_with_anthropic("default", Some(false));
        assert!(!is_1m_context_enabled(&config, "anthropic.default"));
    }

    /// Boundary: flag unset (None) must default to false, not panic.
    #[test]
    fn test_1m_context_defaults_false_when_unset() {
        let config = config_with_anthropic("default", None);
        assert!(!is_1m_context_enabled(&config, "anthropic.default"));
    }

    /// Negative: a non-anthropic provider type never gets the beta, even if
    /// an anthropic entry with the flag happens to exist in the same config.
    #[test]
    fn test_1m_context_false_for_non_anthropic_provider() {
        let config = config_with_anthropic("default", Some(true));
        assert!(!is_1m_context_enabled(&config, "gemini.default"));
        assert!(!is_1m_context_enabled(&config, "openai.default"));
        assert!(!is_1m_context_enabled(&config, "embedded.qwen3-big"));
        assert!(!is_1m_context_enabled(&config, "databricks.default"));
    }

    /// Negative: correct provider type but the named config isn't in the map.
    #[test]
    fn test_1m_context_false_for_unknown_config_name() {
        let config = config_with_anthropic("default", Some(true));
        assert!(!is_1m_context_enabled(&config, "anthropic.nonexistent"));
    }

    /// Boundary: bare "anthropic" resolves to the "default" config name.
    #[test]
    fn test_1m_context_bare_provider_name_resolves_to_default() {
        let config = config_with_anthropic("default", Some(true));
        assert!(is_1m_context_enabled(&config, "anthropic"));
    }

    /// A non-"default" named config is honoured independently.
    #[test]
    fn test_1m_context_custom_config_name() {
        let config = config_with_anthropic("bigctx", Some(true));
        assert!(is_1m_context_enabled(&config, "anthropic.bigctx"));
        // ...and the absent "default" entry is still false
        assert!(!is_1m_context_enabled(&config, "anthropic.default"));
    }

    // ── #fallback suffix stripping ─────────────────────────────────────
    //
    // The fallback provider is registered under the default provider's name
    // plus "#fallback". Every config lookup in this module goes through
    // parse_provider_ref, so stripping the suffix here is what gives the
    // fallback capability parity with the default.

    #[test]
    fn test_parse_provider_ref_strips_fallback_suffix() {
        let (ptype, name) = parse_provider_ref("anthropic.default#fallback");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "default");
    }

    #[test]
    fn test_parse_provider_ref_strips_fallback_suffix_custom_name() {
        let (ptype, name) = parse_provider_ref("openai.gpt4#fallback");
        assert_eq!(ptype, "openai");
        assert_eq!(name, "gpt4");
    }

    /// Boundary: bare provider name plus the suffix still resolves to "default".
    #[test]
    fn test_parse_provider_ref_bare_name_with_fallback_suffix() {
        let (ptype, name) = parse_provider_ref("anthropic#fallback");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "default");
    }

    /// Negative: only a TRAILING suffix is stripped. A '#' appearing elsewhere
    /// must be left alone rather than mangling the reference.
    #[test]
    fn test_parse_provider_ref_hash_not_at_end_is_untouched() {
        let (ptype, name) = parse_provider_ref("anthropic.we#fallbackird");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "we#fallbackird");
    }

    /// Negative: a similar-looking but different suffix is not stripped.
    #[test]
    fn test_parse_provider_ref_similar_suffix_not_stripped() {
        let (ptype, name) = parse_provider_ref("anthropic.default#fallbacks");
        assert_eq!(ptype, "anthropic");
        assert_eq!(name, "default#fallbacks");
    }

    /// The whole point, end to end: the fallback provider name resolves to the
    /// SAME config values as the default it was cloned from.
    #[test]
    fn test_fallback_provider_inherits_default_config_lookups() {
        let mut config = config_with_anthropic("default", Some(true));
        // Give the entry distinctive values we can detect.
        if let Some(entry) = config.providers.anthropic.get_mut("default") {
            entry.max_tokens = Some(54321);
            entry.thinking_budget_tokens = Some(9000);
        }

        let default_ref = "anthropic.default";
        let fallback_ref = "anthropic.default#fallback";

        assert_eq!(
            get_max_tokens(&config, fallback_ref),
            get_max_tokens(&config, default_ref)
        );
        assert_eq!(get_max_tokens(&config, fallback_ref), Some(54321));
        assert_eq!(
            get_thinking_budget_tokens(&config, fallback_ref),
            Some(9000)
        );
        assert!(
            is_1m_context_enabled(&config, fallback_ref),
            "fallback must inherit the 1M-context beta, or context accounting diverges from the API"
        );
        assert_eq!(
            resolve_max_tokens(&config, fallback_ref),
            resolve_max_tokens(&config, default_ref)
        );
    }
}
