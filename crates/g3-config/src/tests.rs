#[cfg(test)]
mod tests {
    use crate::Config;
    use std::fs;
    use tempfile::TempDir;

    fn test_config_footer() -> &'static str {
        r#"
[computer_control]
enabled = false
require_confirmation = true
max_actions_per_second = 10

[webdriver]
enabled = false
safari_port = 4444
"#
    }

    #[test]
    fn test_coach_player_providers() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration with coach and player providers (new format)
        let config_content = format!(r#"
[providers]
default_provider = "databricks.default"
coach = "anthropic.default"
player = "embedded.local"

[providers.databricks.default]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[providers.anthropic.default]
api_key = "test-key"
model = "claude-3"

[providers.embedded.local]
model_path = "test.gguf"
model_type = "llama"

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();

        // Test that the providers are correctly identified
        assert_eq!(config.providers.default_provider, "databricks.default");
        assert_eq!(config.get_coach_provider(), "anthropic.default");
        assert_eq!(config.get_player_provider(), "embedded.local");

        // Test creating coach config
        let coach_config = config.for_coach().unwrap();
        assert_eq!(coach_config.providers.default_provider, "anthropic.default");

        // Test creating player config
        let player_config = config.for_player().unwrap();
        assert_eq!(player_config.providers.default_provider, "embedded.local");
    }

    #[test]
    fn test_coach_player_fallback_to_default() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration WITHOUT coach and player providers (new format)
        let config_content = format!(r#"
[providers]
default_provider = "databricks.default"

[providers.databricks.default]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();

        // Test that coach and player fall back to default provider
        assert_eq!(config.get_coach_provider(), "databricks.default");
        assert_eq!(config.get_player_provider(), "databricks.default");

        // Test creating coach config (should use default)
        let coach_config = config.for_coach().unwrap();
        assert_eq!(coach_config.providers.default_provider, "databricks.default");

        // Test creating player config (should use default)
        let player_config = config.for_player().unwrap();
        assert_eq!(player_config.providers.default_provider, "databricks.default");
    }

    #[test]
    fn test_invalid_provider_error() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration with an unconfigured provider (new format)
        let config_content = format!(r#"
[providers]
default_provider = "databricks.default"
coach = "openai.default"  # OpenAI default is not configured

[providers.databricks.default]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();

        // Test that trying to create a coach config with unconfigured provider fails
        let result = config.for_coach();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not found") || err_msg.contains("not configured"), 
            "Expected error message to contain 'not found' or 'not configured', got: {}", err_msg);
    }

    #[test]
    fn test_old_format_detection() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration with OLD format (api_key directly under [providers.anthropic])
        let config_content = format!(r#"
[providers]
default_provider = "anthropic"

[providers.anthropic]
api_key = "test-key"
model = "claude-3"

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Loading should fail with old format error
        let result = Config::load(Some(config_path.to_str().unwrap()));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("old format") || err_msg.contains("no longer supported"),
            "Expected error about old format, got: {}", err_msg);
    }

    #[test]
    fn test_planner_provider() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration with planner provider (new format)
        let config_content = format!(r#"
[providers]
default_provider = "databricks.default"
planner = "anthropic.planner"

[providers.databricks.default]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[providers.anthropic.planner]
api_key = "test-key"
model = "claude-opus"
thinking_budget_tokens = 16000

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();

        // Test that the planner provider is correctly identified
        assert_eq!(config.get_planner_provider(), "anthropic.planner");

        // Test creating planner config
        let planner_config = config.for_planner().unwrap();
        assert_eq!(planner_config.providers.default_provider, "anthropic.planner");
    }

    #[test]
    fn test_planner_fallback_to_default() {
        // Create a temporary directory for the test config
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        // Write a test configuration WITHOUT planner provider
        let config_content = format!(r#"
[providers]
default_provider = "databricks.default"

[providers.databricks.default]
host = "https://test.databricks.com"
token = "test-token"
model = "test-model"

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        // Load the configuration
        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();

        // Test that planner falls back to default provider
        assert_eq!(config.get_planner_provider(), "databricks.default");
    }

    #[test]
    fn test_openai_compatible_reasoning_exclude_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        let config_content = format!(r#"
[providers]
default_provider = "openrouter.default"

[providers.openai_compatible.openrouter]
api_key = "test-key"
model = "x-ai/grok-code-fast-1"
base_url = "https://openrouter.ai/api/v1"
reasoning_exclude = true

[agent]
fallback_default_max_tokens = 32000
enable_streaming = true
timeout_seconds = 60
auto_compact = true
max_retry_attempts = 3
autonomous_max_retry_attempts = 6
{}"#, test_config_footer());

        fs::write(&config_path, config_content).unwrap();

        let config = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        let openrouter = config.providers.openai_compatible.get("openrouter").unwrap();

        assert_eq!(openrouter.reasoning_exclude, Some(true));
    }
}
