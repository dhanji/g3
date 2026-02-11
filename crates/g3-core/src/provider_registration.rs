//! Provider registration logic for the Agent.
//!
//! This module handles the registration of LLM providers (Anthropic, OpenAI, Databricks, Embedded)
//! based on configuration. It consolidates the duplicated registration patterns into a single
//! cohesive module.

use anyhow::Result;
use g3_config::Config;
use g3_providers::ProviderRegistry;
use tracing::debug;

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

    Ok(registry)
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
            debug!(
                "Registering Anthropic provider '{}' with base_url: {:?}",
                name, anthropic_config.base_url
            );
            let anthropic_provider = g3_providers::AnthropicProvider::new_with_name(
                format!("anthropic.{}", name),
                anthropic_config.api_key.clone(),
                Some(anthropic_config.model.clone()),
                anthropic_config.base_url.clone(),
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
