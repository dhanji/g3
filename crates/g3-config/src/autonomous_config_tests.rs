#[cfg(test)]
mod autonomous_config_tests {
    use crate::{Config, AnthropicConfig, DatabricksConfig};

    #[test]
    fn test_default_autonomous_config() {
        let config = Config::default();
        assert!(config.autonomous.coach_provider.is_none());
        assert!(config.autonomous.coach_model.is_none());
        assert!(config.autonomous.player_provider.is_none());
        assert!(config.autonomous.player_model.is_none());
    }

    #[test]
    fn test_for_coach_with_overrides() {
        let mut config = Config::default();
        
        // Set up base config with anthropic
        config.providers.anthropic = Some(AnthropicConfig {
            api_key: "test-key".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            max_tokens: Some(4096),
            temperature: Some(0.1),
        });
        
        // Set coach overrides
        config.autonomous.coach_provider = Some("anthropic".to_string());
        config.autonomous.coach_model = Some("claude-3-opus-20240229".to_string());
        
        let coach_config = config.for_coach().unwrap();
        
        // Verify coach uses overridden provider and model
        assert_eq!(coach_config.providers.default_provider, "anthropic");
        assert_eq!(
            coach_config.providers.anthropic.as_ref().unwrap().model,
            "claude-3-opus-20240229"
        );
    }

    #[test]
    fn test_for_player_with_overrides() {
        let mut config = Config::default();
        
        // Set up base config with databricks
        config.providers.databricks = Some(DatabricksConfig {
            host: "https://test.databricks.com".to_string(),
            token: Some("test-token".to_string()),
            model: "databricks-meta-llama-3-1-70b-instruct".to_string(),
            max_tokens: Some(4096),
            temperature: Some(0.1),
            use_oauth: Some(false),
        });
        
        // Set player overrides
        config.autonomous.player_provider = Some("databricks".to_string());
        config.autonomous.player_model = Some("databricks-dbrx-instruct".to_string());
        
        let player_config = config.for_player().unwrap();
        
        // Verify player uses overridden provider and model
        assert_eq!(player_config.providers.default_provider, "databricks");
        assert_eq!(
            player_config.providers.databricks.as_ref().unwrap().model,
            "databricks-dbrx-instruct"
        );
    }

    #[test]
    fn test_no_overrides_uses_defaults() {
        let mut config = Config::default();
        config.providers.default_provider = "databricks".to_string();
        
        let coach_config = config.for_coach().unwrap();
        let player_config = config.for_player().unwrap();
        
        // Both should use the default provider when no overrides
        assert_eq!(coach_config.providers.default_provider, "databricks");
        assert_eq!(player_config.providers.default_provider, "databricks");
    }

    #[test]
    fn test_provider_override_only() {
        let mut config = Config::default();
        
        config.providers.anthropic = Some(AnthropicConfig {
            api_key: "test-key".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            max_tokens: Some(4096),
            temperature: Some(0.1),
        });
        
        // Only override provider, not model
        config.autonomous.coach_provider = Some("anthropic".to_string());
        
        let coach_config = config.for_coach().unwrap();
        
        // Should use overridden provider with its default model
        assert_eq!(coach_config.providers.default_provider, "anthropic");
        assert_eq!(
            coach_config.providers.anthropic.as_ref().unwrap().model,
            "claude-3-5-sonnet-20241022"
        );
    }

    #[test]
    fn test_model_override_only() {
        let mut config = Config::default();
        config.providers.default_provider = "databricks".to_string();
        
        config.providers.databricks = Some(DatabricksConfig {
            host: "https://test.databricks.com".to_string(),
            token: Some("test-token".to_string()),
            model: "databricks-meta-llama-3-1-70b-instruct".to_string(),
            max_tokens: Some(4096),
            temperature: Some(0.1),
            use_oauth: Some(false),
        });
        
        // Only override model, not provider
        config.autonomous.player_model = Some("databricks-dbrx-instruct".to_string());
        
        let player_config = config.for_player().unwrap();
        
        // Should use default provider with overridden model
        assert_eq!(player_config.providers.default_provider, "databricks");
        assert_eq!(
            player_config.providers.databricks.as_ref().unwrap().model,
            "databricks-dbrx-instruct"
        );
    }
}
