//! Provider registration logic for the Agent.
//!
//! This module handles the registration of LLM providers (Anthropic, OpenAI, Databricks, Embedded)
//! based on configuration. It consolidates the duplicated registration patterns into a single
//! cohesive module.

use anyhow::Result;
use g3_config::Config;
use g3_providers::{ProviderRegistry, FALLBACK_PROVIDER_SUFFIX};
use tracing::{debug, warn};

/// Determines which providers should be registered based on mode and configuration.
///
/// In autonomous mode, registers coach and player providers in addition to the default.
/// In normal mode, only registers the default provider.
pub fn determine_providers_to_register(config: &Config, is_autonomous: bool) -> Vec<String> {
    if is_autonomous {
        let mut providers = vec![config.providers.default_provider.clone()];
        if let Some(coach) = &config.providers.coach {
            if !providers.contains(coach) {
                providers.push(coach.clone());
            }
        }
        if let Some(player) = &config.providers.player {
            if !providers.contains(player) {
                providers.push(player.clone());
            }
        }
        providers
    } else {
        vec![config.providers.default_provider.clone()]
    }
}

/// Checks if a provider reference should be registered.
///
/// A provider should be registered if:
/// - Its full reference (e.g., "openai.default") is in the list, OR
/// - Any provider of that type is in the list (e.g., "openai.*")
fn should_register(providers_to_register: &[String], provider_type: &str, config_name: &str) -> bool {
    let full_ref = format!("{}.{}", provider_type, config_name);
    providers_to_register
        .iter()
        .any(|p| p == &full_ref || p.starts_with(&format!("{}.", provider_type)))
}

/// Registers all configured providers based on the providers_to_register list.
///
/// This is an async function because Databricks OAuth registration requires async.
pub async fn register_providers(
    config: &Config,
    providers_to_register: &[String],
) -> Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::new();

    register_embedded_providers(config, providers_to_register, &mut registry)?;
    register_openai_providers(config, providers_to_register, &mut registry)?;
    register_openai_compatible_providers(config, providers_to_register, &mut registry)?;
    register_anthropic_providers(config, providers_to_register, &mut registry)?;
    register_gemini_providers(config, providers_to_register, &mut registry)?;
    register_databricks_providers(config, providers_to_register, &mut registry).await?;

    // Set default provider
    debug!(
        "Setting default provider to: {}",
        config.providers.default_provider
    );
    registry.set_default(&config.providers.default_provider)?;
    debug!("Default provider set successfully");

    // Register the overload fallback last, so it can be built from the same
    // config entry the default provider was built from.
    register_fallback_provider(config, &mut registry)?;

    Ok(registry)
}

/// Register the overload fallback provider, if `providers.fallback_model` is set.
///
/// The fallback is the default provider's OWN config entry with the model
/// string swapped — same api key, base url, host, cache config, thinking
/// budget, 1M-context beta, max_tokens and temperature. Cloning the entry
/// rather than synthesising a new one is what guarantees capability parity: a
/// hand-built fallback would silently drift every time a new provider option
/// was added.
///
/// Returns Ok(()) without registering anything when the feature is off, when
/// the fallback model equals the default model (nothing to fall back to), or
/// when the default provider type cannot meaningfully take a model override.
fn register_fallback_provider(config: &Config, registry: &mut ProviderRegistry) -> Result<()> {
    let Some(fallback_model) = config.providers.fallback_model.as_deref() else {
        return Ok(()); // Feature off — behave exactly as before.
    };

    if fallback_model.trim().is_empty() {
        warn!("Ignoring empty --fallback-model value");
        return Ok(());
    }

    let default_ref = config.providers.default_provider.clone();
    let (provider_type, config_name) = Config::parse_provider_reference(&default_ref)?;
    let fallback_name = format!("{}{}", default_ref, FALLBACK_PROVIDER_SUFFIX);

    match provider_type.as_str() {
        "anthropic" => {
            let Some(base) = config.providers.anthropic.get(&config_name) else {
                warn!("Cannot register fallback model: anthropic.{} not found", config_name);
                return Ok(());
            };
            if base.model == fallback_model {
                debug!("Fallback model equals default model ({}); no fallback registered", fallback_model);
                return Ok(());
            }
            let provider = g3_providers::AnthropicProvider::new_with_name(
                fallback_name.clone(),
                base.api_key.clone(),
                Some(fallback_model.to_string()),
                base.max_tokens,
                base.temperature,
                base.cache_config.clone(),
                base.enable_1m_context,
                base.thinking_budget_tokens,
            )?;
            registry.register_fallback(provider);
        }
        "openai" => {
            let Some(base) = config.providers.openai.get(&config_name) else {
                warn!("Cannot register fallback model: openai.{} not found", config_name);
                return Ok(());
            };
            if base.model == fallback_model {
                debug!("Fallback model equals default model ({}); no fallback registered", fallback_model);
                return Ok(());
            }
            let provider = g3_providers::OpenAIProvider::new_with_name(
                fallback_name.clone(),
                base.api_key.clone(),
                Some(fallback_model.to_string()),
                base.base_url.clone(),
                base.max_tokens,
                base.temperature,
            )?;
            registry.register_fallback(provider);
        }
        "gemini" => {
            let Some(base) = config.providers.gemini.get(&config_name) else {
                warn!("Cannot register fallback model: gemini.{} not found", config_name);
                return Ok(());
            };
            if base.model == fallback_model {
                debug!("Fallback model equals default model ({}); no fallback registered", fallback_model);
                return Ok(());
            }
            let provider = g3_providers::GeminiProvider::new_with_name(
                fallback_name.clone(),
                base.api_key.clone(),
                Some(fallback_model.to_string()),
                base.max_tokens,
                base.temperature,
            )?;
            registry.register_fallback(provider);
        }
        "databricks" => {
            let Some(base) = config.providers.databricks.get(&config_name) else {
                warn!("Cannot register fallback model: databricks.{} not found", config_name);
                return Ok(());
            };
            if base.model == fallback_model {
                debug!("Fallback model equals default model ({}); no fallback registered", fallback_model);
                return Ok(());
            }
            // Only the token path is supported here. The OAuth constructor is
            // async and performs a network round trip; doing that eagerly for a
            // provider that may never be used would tax every startup.
            let Some(token) = base.token.as_ref() else {
                warn!(
                    "Fallback model not registered: databricks.{} uses OAuth, which cannot be \
                     initialised lazily. Set a token to use --fallback-model.",
                    config_name
                );
                return Ok(());
            };
            let provider = g3_providers::DatabricksProvider::from_token_with_name(
                fallback_name.clone(),
                base.host.clone(),
                token.clone(),
                fallback_model.to_string(),
                base.max_tokens,
                base.temperature,
            )?;
            registry.register_fallback(provider);
        }
        "embedded" => {
            // A model here is a path to a multi-gigabyte GGUF that would be
            // loaded into memory at startup. An overload fallback makes no
            // sense for a local model anyway: local inference does not return
            // "overloaded".
            warn!(
                "--fallback-model is not supported for embedded providers ({}); ignoring",
                default_ref
            );
            return Ok(());
        }
        other => {
            // openai_compatible providers are keyed by their own name.
            let Some(base) = config.providers.openai_compatible.get(other) else {
                warn!("Cannot register fallback model: unknown provider type '{}'", other);
                return Ok(());
            };
            if base.model == fallback_model {
                debug!("Fallback model equals default model ({}); no fallback registered", fallback_model);
                return Ok(());
            }
            let provider = g3_providers::OpenAIProvider::new_with_name(
                fallback_name.clone(),
                base.api_key.clone(),
                Some(fallback_model.to_string()),
                base.base_url.clone(),
                base.max_tokens,
                base.temperature,
            )?;
            registry.register_fallback(provider);
        }
    }

    debug!(
        "Registered overload fallback provider '{}' (model={})",
        fallback_name, fallback_model
    );
    Ok(())
}

/// Register embedded providers from configuration.
fn register_embedded_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, embedded_config) in &config.providers.embedded {
        if should_register(providers_to_register, "embedded", name) {
            let embedded_provider = g3_providers::EmbeddedProvider::new_with_name(
                format!("embedded.{}", name),
                embedded_config.model_path.clone(),
                embedded_config.model_type.clone(),
                embedded_config.context_length,
                embedded_config.max_tokens,
                embedded_config.temperature,
                embedded_config.gpu_layers,
                embedded_config.threads,
            )?;
            registry.register(embedded_provider);
        }
    }
    Ok(())
}

/// Register OpenAI providers from configuration.
fn register_openai_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, openai_config) in &config.providers.openai {
        if should_register(providers_to_register, "openai", name) {
            let openai_provider = g3_providers::OpenAIProvider::new_with_name(
                format!("openai.{}", name),
                openai_config.api_key.clone(),
                Some(openai_config.model.clone()),
                openai_config.base_url.clone(),
                openai_config.max_tokens,
                openai_config.temperature,
            )?;
            registry.register(openai_provider);
        }
    }
    Ok(())
}

/// Register OpenAI-compatible providers (e.g., OpenRouter, Groq) from configuration.
fn register_openai_compatible_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, openai_config) in &config.providers.openai_compatible {
        if should_register(providers_to_register, name, "default") {
            let openai_provider = g3_providers::OpenAIProvider::new_with_name(
                name.clone(),
                openai_config.api_key.clone(),
                Some(openai_config.model.clone()),
                openai_config.base_url.clone(),
                openai_config.max_tokens,
                openai_config.temperature,
            )?;
            registry.register(openai_provider);
        }
    }
    Ok(())
}

/// Register Anthropic providers from configuration.
fn register_anthropic_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, anthropic_config) in &config.providers.anthropic {
        if should_register(providers_to_register, "anthropic", name) {
            let anthropic_provider = g3_providers::AnthropicProvider::new_with_name(
                format!("anthropic.{}", name),
                anthropic_config.api_key.clone(),
                Some(anthropic_config.model.clone()),
                anthropic_config.max_tokens,
                anthropic_config.temperature,
                anthropic_config.cache_config.clone(),
                anthropic_config.enable_1m_context,
                anthropic_config.thinking_budget_tokens,
            )?;
            registry.register(anthropic_provider);
        }
    }
    Ok(())
}

/// Register Gemini providers from configuration.
fn register_gemini_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, gemini_config) in &config.providers.gemini {
        if should_register(providers_to_register, "gemini", name) {
            let gemini_provider = g3_providers::GeminiProvider::new_with_name(
                format!("gemini.{}", name),
                gemini_config.api_key.clone(),
                Some(gemini_config.model.clone()),
                gemini_config.max_tokens,
                gemini_config.temperature,
            )?;
            registry.register(gemini_provider);
        }
    }
    Ok(())
}

/// Register Databricks providers from configuration.
///
/// This is async because OAuth authentication requires async operations.
async fn register_databricks_providers(
    config: &Config,
    providers_to_register: &[String],
    registry: &mut ProviderRegistry,
) -> Result<()> {
    for (name, databricks_config) in &config.providers.databricks {
        if should_register(providers_to_register, "databricks", name) {
            let databricks_provider = if let Some(token) = &databricks_config.token {
                // Use token-based authentication
                g3_providers::DatabricksProvider::from_token_with_name(
                    format!("databricks.{}", name),
                    databricks_config.host.clone(),
                    token.clone(),
                    databricks_config.model.clone(),
                    databricks_config.max_tokens,
                    databricks_config.temperature,
                )?
            } else {
                // Use OAuth authentication
                g3_providers::DatabricksProvider::from_oauth_with_name(
                    format!("databricks.{}", name),
                    databricks_config.host.clone(),
                    databricks_config.model.clone(),
                    databricks_config.max_tokens,
                    databricks_config.temperature,
                )
                .await?
            };

            registry.register(databricks_provider);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_register_exact_match() {
        let providers = vec!["openai.default".to_string()];
        assert!(should_register(&providers, "openai", "default"));
        // When openai.default is in the list, ALL openai.* providers are registered
        // This is intentional - the original code registered all providers of a type
        assert!(should_register(&providers, "openai", "other"));
        assert!(!should_register(&providers, "anthropic", "default"));
    }

    #[test]
    fn test_should_register_type_prefix() {
        let providers = vec!["openai.gpt4".to_string()];
        // Any openai.* should match when we have openai.gpt4
        assert!(should_register(&providers, "openai", "gpt4"));
        assert!(should_register(&providers, "openai", "other")); // prefix match
        assert!(!should_register(&providers, "anthropic", "default"));
    }

    #[test]
    fn test_determine_providers_normal_mode() {
        // Create a minimal config for testing
        let config = Config::default();
        let providers = determine_providers_to_register(&config, false);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0], config.providers.default_provider);
    }
}

#[cfg(test)]
mod fallback_registration_tests {
    use super::*;
    use g3_config::{AnthropicConfig, Config, OpenAIConfig};

    /// Config whose default provider is `anthropic.default`, with distinctive
    /// capability settings we can assert are inherited.
    fn anthropic_config(fallback_model: Option<&str>) -> Config {
        let mut config = Config::default();
        config.providers.default_provider = "anthropic.default".to_string();
        config.providers.databricks.clear(); // drop Config::default()'s OAuth entry
        config.providers.anthropic.insert(
            "default".to_string(),
            AnthropicConfig {
                api_key: "sk-test-key".to_string(),
                model: "claude-opus-5".to_string(),
                max_tokens: Some(41000),
                temperature: Some(0.3),
                cache_config: Some("ephemeral".to_string()),
                enable_1m_context: Some(true),
                thinking_budget_tokens: None,
            },
        );
        config.providers.fallback_model = fallback_model.map(str::to_string);
        config
    }

    fn register(config: &Config) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        // Register the default provider the same way register_providers does.
        register_anthropic_providers(config, &["anthropic.default".to_string()], &mut registry)
            .unwrap();
        register_openai_providers(config, &["openai.default".to_string()], &mut registry).unwrap();
        if !registry.list_providers().is_empty() {
            let _ = registry.set_default(&config.providers.default_provider);
        }
        register_fallback_provider(config, &mut registry).unwrap();
        registry
    }

    #[test]
    fn test_fallback_registered_with_expected_name_and_model() {
        let config = anthropic_config(Some("claude-opus-4-8"));
        let registry = register(&config);

        assert!(registry.has_fallback());
        assert_eq!(
            registry.fallback_name(),
            Some("anthropic.default#fallback")
        );
        assert_eq!(registry.fallback_model(), Some("claude-opus-4-8"));
    }

    /// Capability parity, via the trait surface.
    ///
    /// Asserts against the CONFIGURED values (41000 / 0.3), not merely against
    /// each other: `max_tokens.unwrap_or(32768)` means a fallback built without
    /// the base entry's settings would still return a plausible-looking number,
    /// and an assertion of `fallback == default` alone would also pass if both
    /// were wrong. The literals are what make this test falsifiable.
    #[test]
    fn test_fallback_inherits_default_capabilities() {
        let config = anthropic_config(Some("claude-opus-4-8"));
        let registry = register(&config);

        let default = registry.get(Some("anthropic.default")).unwrap();
        let fallback = registry.get(Some("anthropic.default#fallback")).unwrap();

        assert_eq!(fallback.max_tokens(), 41000, "must inherit configured max_tokens, not the 32768 provider default");
        assert_eq!(fallback.max_tokens(), default.max_tokens());
        assert!((fallback.temperature() - 0.3).abs() < f32::EPSILON, "must inherit configured temperature, not the 0.1 provider default");
        assert!((fallback.temperature() - default.temperature()).abs() < f32::EPSILON);
        assert_eq!(
            fallback.has_native_tool_calling(),
            default.has_native_tool_calling(),
            "a fallback without native tool calling could not run an agent turn"
        );
        // ...and the ONLY difference is the model.
        assert_ne!(fallback.model(), default.model());
        assert_eq!(fallback.model(), "claude-opus-4-8");
        assert_eq!(default.model(), "claude-opus-5");
    }

    /// SOURCE INSPECTION, deliberately.
    ///
    /// `cache_config`, `enable_1m_context` and `thinking_budget_tokens` are
    /// stored by AnthropicProvider but are NOT exposed on the LLMProvider trait
    /// — `supports_cache_control()` is a hardcoded `true`, and the 1M flag only
    /// changes a request header. So no behavioural assertion through the
    /// registry can detect their loss: a mutation replacing
    /// `base.cache_config.clone()` with `None` passes every runtime test (I
    /// verified this). The bug class is an OMISSION at a call site, which is
    /// exactly the case where inspecting the construction is the only honest
    /// check available.
    #[test]
    fn test_anthropic_fallback_forwards_every_config_field() {
        let source = include_str!("provider_registration.rs");
        let start = source
            .find("fn register_fallback_provider")
            .expect("register_fallback_provider must exist");
        let body = &source[start..];
        let anthropic_arm = &body[..body
            .find("\"openai\" =>")
            .expect("anthropic arm must precede the openai arm")];

        for field in [
            "base.api_key",
            "base.max_tokens",
            "base.temperature",
            "base.cache_config",
            "base.enable_1m_context",
            "base.thinking_budget_tokens",
        ] {
            assert!(
                anthropic_arm.contains(field),
                "the anthropic fallback must forward {field} from the default provider's \
                 config entry; dropping it silently changes behaviour on a fallback turn"
            );
        }
        assert!(
            !anthropic_arm.contains("            None,"),
            "a bare `None` argument in the fallback construction means a config field \
             was dropped rather than inherited"
        );
    }

    /// Feature off: nothing extra registered.
    #[test]
    fn test_no_fallback_model_registers_nothing() {
        let config = anthropic_config(None);
        let registry = register(&config);

        assert!(!registry.has_fallback());
        assert_eq!(registry.fallback_name(), None);
        assert_eq!(registry.list_providers().len(), 1);
    }

    /// Boundary: a fallback identical to the default is pointless — registering
    /// it would make an overload retry hit the same congested pool while
    /// reporting a switch.
    #[test]
    fn test_fallback_equal_to_default_model_registers_nothing() {
        let config = anthropic_config(Some("claude-opus-5"));
        let registry = register(&config);

        assert!(!registry.has_fallback());
    }

    /// Boundary: whitespace-only value is ignored rather than registering a
    /// provider with a nonsense model.
    #[test]
    fn test_blank_fallback_model_registers_nothing() {
        let config = anthropic_config(Some("   "));
        let registry = register(&config);

        assert!(!registry.has_fallback());
    }

    /// Negative: embedded default provider is skipped with a warning, NOT an
    /// error — a bad flag must never prevent g3 from starting.
    #[test]
    fn test_embedded_default_provider_is_skipped_not_an_error() {
        let mut config = Config::default();
        config.providers.default_provider = "embedded.qwen3-big".to_string();
        config.providers.fallback_model = Some("claude-opus-4-8".to_string());

        let mut registry = ProviderRegistry::new();
        let result = register_fallback_provider(&config, &mut registry);

        assert!(result.is_ok(), "must not error: {:?}", result.err());
        assert!(!registry.has_fallback());
    }

    /// Negative: default provider reference names a config entry that does not
    /// exist. Skip quietly rather than failing startup.
    #[test]
    fn test_missing_default_config_entry_is_skipped_not_an_error() {
        let mut config = anthropic_config(Some("claude-opus-4-8"));
        config.providers.default_provider = "anthropic.nonexistent".to_string();

        let mut registry = ProviderRegistry::new();
        let result = register_fallback_provider(&config, &mut registry);

        assert!(result.is_ok(), "must not error: {:?}", result.err());
        assert!(!registry.has_fallback());
    }

    /// Negative: a Databricks default using OAuth cannot be cloned lazily, so
    /// it is skipped with a warning instead of blocking startup on a network
    /// round trip.
    #[test]
    fn test_databricks_oauth_default_is_skipped_not_an_error() {
        let mut config = Config::default(); // default entry uses OAuth, token: None
        config.providers.default_provider = "databricks.default".to_string();
        config.providers.fallback_model = Some("databricks-claude-opus".to_string());

        let mut registry = ProviderRegistry::new();
        let result = register_fallback_provider(&config, &mut registry);

        assert!(result.is_ok(), "must not error: {:?}", result.err());
        assert!(!registry.has_fallback());
    }

    /// A non-anthropic provider type also works (openai here), proving the
    /// feature is not hardcoded to one vendor.
    #[test]
    fn test_openai_default_gets_a_fallback() {
        let mut config = Config::default();
        config.providers.default_provider = "openai.default".to_string();
        config.providers.databricks.clear();
        config.providers.openai.insert(
            "default".to_string(),
            OpenAIConfig {
                api_key: "sk-openai".to_string(),
                model: "gpt-5".to_string(),
                base_url: Some("https://api.openai.com/v1".to_string()),
                max_tokens: Some(30000),
                temperature: Some(0.2),
            },
        );
        config.providers.fallback_model = Some("gpt-5-mini".to_string());

        let mut registry = ProviderRegistry::new();
        register_openai_providers(&config, &["openai.default".to_string()], &mut registry).unwrap();
        registry.set_default("openai.default").unwrap();
        register_fallback_provider(&config, &mut registry).unwrap();

        assert_eq!(registry.fallback_name(), Some("openai.default#fallback"));
        assert_eq!(registry.fallback_model(), Some("gpt-5-mini"));
    }

    /// The fallback is registered but INACTIVE at startup: a fresh process must
    /// begin on the default model.
    #[test]
    fn test_fallback_is_inactive_after_registration() {
        let config = anthropic_config(Some("claude-opus-4-8"));
        let registry = register(&config);

        assert!(!registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
    }
}
