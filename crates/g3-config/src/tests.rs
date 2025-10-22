#[cfg(test)]
mod tests {
    use crate::Config;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_coach_player_providers() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");
        
        // Write a test configuration with coach and player providers
        let config_content = r#"
[providers]
default_provider = "databricks"
coach = "anthropic"
player = "embedded"

[providers.databricks]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[providers.anthropic]
api_key = "test-key"
model = "claude-3"

[providers.embedded]
model_path = "test.gguf"
model_type = "llama"

[agent]
max_context_length = 8192
enable_streaming = true
timeout_seconds = 60
"#;
        
        fs::write(&config_path, config_content).unwrap();
        
        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        
        // Test that the providers are correctly identified
        assert_eq!(config.providers.default_provider, "databricks");
        assert_eq!(config.get_coach_provider(), "anthropic");
        assert_eq!(config.get_player_provider(), "embedded");
        
        // Test creating coach config
        let coach_config = config.for_coach().unwrap();
        assert_eq!(coach_config.providers.default_provider, "anthropic");
        
        // Test creating player config
        let player_config = config.for_player().unwrap();
        assert_eq!(player_config.providers.default_provider, "embedded");
    }
    
    #[test]
    fn test_coach_player_fallback_to_default() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");
        
        // Write a test configuration WITHOUT coach and player providers
        let config_content = r#"
[providers]
default_provider = "databricks"

[providers.databricks]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[agent]
max_context_length = 8192
enable_streaming = true
timeout_seconds = 60
"#;
        
        fs::write(&config_path, config_content).unwrap();
        
        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        
        // Test that coach and player fall back to default provider
        assert_eq!(config.get_coach_provider(), "databricks");
        assert_eq!(config.get_player_provider(), "databricks");
        
        // Test creating coach config (should use default)
        let coach_config = config.for_coach().unwrap();
        assert_eq!(coach_config.providers.default_provider, "databricks");
        
        // Test creating player config (should use default)
        let player_config = config.for_player().unwrap();
        assert_eq!(player_config.providers.default_provider, "databricks");
    }
    
    #[test]
    fn test_invalid_provider_error() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");
        
        // Write a test configuration with an unconfigured provider
        let config_content = r#"
[providers]
default_provider = "databricks"
coach = "openai"  # OpenAI is not configured

[providers.databricks]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[agent]
max_context_length = 8192
enable_streaming = true
timeout_seconds = 60
"#;
        
        fs::write(&config_path, config_content).unwrap();
        
        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        
        // Test that trying to create a coach config with unconfigured provider fails
        let result = config.for_coach();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not configured"));
    }
}