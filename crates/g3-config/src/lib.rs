use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub providers: ProvidersConfig,
    pub agent: AgentConfig,
    pub computer_control: ComputerControlConfig,
    pub webdriver: WebDriverConfig,
    pub macax: MacAxConfig,
}

/// Provider configuration with named configs per provider type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// Default provider in format "<provider_type>.<config_name>"
    pub default_provider: String,
    
    /// Provider for planner mode (optional, falls back to default_provider)
    pub planner: Option<String>,
    
    /// Provider for coach in autonomous mode (optional, falls back to default_provider)
    pub coach: Option<String>,
    
    /// Provider for player in autonomous mode (optional, falls back to default_provider)
    pub player: Option<String>,
    
    /// Named Anthropic provider configs
    #[serde(default)]
    pub anthropic: HashMap<String, AnthropicConfig>,
    
    /// Named OpenAI provider configs
    #[serde(default)]
    pub openai: HashMap<String, OpenAIConfig>,
    
    /// Named Databricks provider configs
    #[serde(default)]
    pub databricks: HashMap<String, DatabricksConfig>,
    
    /// Named embedded provider configs
    #[serde(default)]
    pub embedded: HashMap<String, EmbeddedConfig>,
    
    /// Multiple named OpenAI-compatible providers (e.g., openrouter, groq, etc.)
    #[serde(default)]
    pub openai_compatible: HashMap<String, OpenAIConfig>,
    
    /// Named Azure OpenAI provider configs (for Claude via Azure AI)
    #[serde(default)]
    pub azure: HashMap<String, AzureConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub cache_config: Option<String>,
    pub enable_1m_context: Option<bool>,
    pub thinking_budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksConfig {
    pub host: String,
    pub token: Option<String>,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub use_oauth: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedConfig {
    pub model_path: String,
    pub model_type: String,
    pub context_length: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub gpu_layers: Option<u32>,
    pub threads: Option<u32>,
}

/// Azure OpenAI configuration
/// For accessing Claude models via Azure AI Model Catalog
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzureConfig {
    /// Azure resource endpoint (e.g., "https://your-resource.openai.azure.com")
    pub endpoint: String,
    /// Azure API key
    pub api_key: String,
    /// Deployment name (the name you gave when deploying the model)
    pub deployment: String,
    /// API version (e.g., "2024-02-15-preview")
    #[serde(default = "default_azure_api_version")]
    pub api_version: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

fn default_azure_api_version() -> String {
    "2024-02-15-preview".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub max_context_length: Option<u32>,
    pub fallback_default_max_tokens: usize,
    pub enable_streaming: bool,
    pub allow_multiple_tool_calls: bool,
    pub timeout_seconds: u64,
    pub auto_compact: bool,
    pub max_retry_attempts: u32,
    pub autonomous_max_retry_attempts: u32,
    #[serde(default = "default_check_todo_staleness")]
    pub check_todo_staleness: bool,
}

fn default_check_todo_staleness() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerControlConfig {
    pub enabled: bool,
    pub require_confirmation: bool,
    pub max_actions_per_second: u32,
}

/// Browser type for WebDriver
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebDriverBrowser {
    #[default]
    Safari,
    #[serde(rename = "chrome-headless")]
    ChromeHeadless,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDriverConfig {
    pub enabled: bool,
    pub safari_port: u16,
    #[serde(default)]
    pub chrome_port: u16,
    #[serde(default)]
    /// Optional path to Chrome binary (e.g., Chrome for Testing)
    /// If not set, ChromeDriver will use the default Chrome installation
    pub chrome_binary: Option<String>,
    #[serde(default)]
    pub browser: WebDriverBrowser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacAxConfig {
    pub enabled: bool,
}

impl Default for MacAxConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

impl Default for WebDriverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            safari_port: 4444,
            chrome_port: 9515,
            chrome_binary: None,
            browser: WebDriverBrowser::Safari,
        }
    }
}

impl Default for ComputerControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_confirmation: true,
            max_actions_per_second: 5,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut databricks_configs = HashMap::new();
        databricks_configs.insert(
            "default".to_string(),
            DatabricksConfig {
                host: "https://your-workspace.cloud.databricks.com".to_string(),
                token: None,
                model: "databricks-claude-sonnet-4".to_string(),
                max_tokens: Some(4096),
                temperature: Some(0.1),
                use_oauth: Some(true),
            },
        );

        Self {
            providers: ProvidersConfig {
                default_provider: "databricks.default".to_string(),
                planner: None,
                coach: None,
                player: None,
                anthropic: HashMap::new(),
                openai: HashMap::new(),
                databricks: databricks_configs,
                embedded: HashMap::new(),
                openai_compatible: HashMap::new(),
                azure: HashMap::new(),
            },
            agent: AgentConfig {
                max_context_length: None,
                fallback_default_max_tokens: 8192,
                enable_streaming: true,
                allow_multiple_tool_calls: false,
                timeout_seconds: 60,
                auto_compact: true,
                max_retry_attempts: 3,
                autonomous_max_retry_attempts: 6,
                check_todo_staleness: true,
            },
            computer_control: ComputerControlConfig::default(),
            webdriver: WebDriverConfig::default(),
            macax: MacAxConfig::default(),
        }
    }
}

/// Error message for old config format
const OLD_CONFIG_FORMAT_ERROR: &str = r#"Your configuration file uses an old format that is no longer supported.

Please update your configuration to use the new provider format:

```toml
[providers]
default_provider = "anthropic.default"  # Format: "<provider_type>.<config_name>"
planner = "anthropic.planner"           # Optional: specific provider for planner
coach = "anthropic.default"             # Optional: specific provider for coach  
player = "openai.player"                # Optional: specific provider for player

# Named configs per provider type
[providers.anthropic.default]
api_key = "your-api-key"
model = "claude-sonnet-4-5"
max_tokens = 64000

[providers.anthropic.planner]
api_key = "your-api-key"
model = "claude-opus-4-5"
thinking_budget_tokens = 16000

[providers.openai.player]
api_key = "your-api-key"
model = "gpt-5"
```

Each mode (planner, coach, player) can specify a full path like "<provider_type>.<config_name>".
If not specified, they fall back to `default_provider`."#;

impl Config {
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        // Check if any config file exists
        let config_exists = if let Some(path) = config_path {
            Path::new(path).exists()
        } else {
            let default_paths = ["./g3.toml", "~/.config/g3/config.toml", "~/.g3.toml"];
            default_paths.iter().any(|path| {
                let expanded_path = shellexpand::tilde(path);
                Path::new(expanded_path.as_ref()).exists()
            })
        };

        // If no config exists, create and save a default config
        if !config_exists {
            let default_config = Self::default();

            let config_dir = dirs::home_dir()
                .map(|mut path| {
                    path.push(".config");
                    path.push("g3");
                    path
                })
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            std::fs::create_dir_all(&config_dir).ok();

            let config_file = config_dir.join("config.toml");
            if let Err(e) = default_config.save(config_file.to_str().unwrap()) {
                eprintln!("Warning: Could not save default config: {}", e);
            } else {
                println!(
                    "Created default configuration at: {}",
                    config_file.display()
                );
            }

            return Ok(default_config);
        }

        // Load config from file
        let config_path_to_load = if let Some(path) = config_path {
            Some(path.to_string())
        } else {
            let default_paths = ["./g3.toml", "~/.config/g3/config.toml", "~/.g3.toml"];
            default_paths.iter().find_map(|path| {
                let expanded_path = shellexpand::tilde(path);
                if Path::new(expanded_path.as_ref()).exists() {
                    Some(expanded_path.to_string())
                } else {
                    None
                }
            })
        };

        if let Some(path) = config_path_to_load {
            // Read and parse the config file
            let config_content = std::fs::read_to_string(&path)?;
            
            // Check for old format (direct provider config without named configs)
            if Self::is_old_format(&config_content) {
                anyhow::bail!("{}", OLD_CONFIG_FORMAT_ERROR);
            }
            
            let config: Config = toml::from_str(&config_content)?;
            
            // Validate the default_provider format
            config.validate_provider_reference(&config.providers.default_provider)?;
            
            return Ok(config);
        }

        Ok(Self::default())
    }

    /// Check if the config content uses the old format
    fn is_old_format(content: &str) -> bool {
        // Old format has [providers.anthropic] with api_key directly
        // New format has [providers.anthropic.<name>] with api_key
        
        // Parse as TOML value to inspect structure
        if let Ok(value) = content.parse::<toml::Value>() {
            if let Some(providers) = value.get("providers") {
                if let Some(providers_table) = providers.as_table() {
                    // Check anthropic section
                    if let Some(anthropic) = providers_table.get("anthropic") {
                        if let Some(anthropic_table) = anthropic.as_table() {
                            // If anthropic has api_key directly, it's old format
                            if anthropic_table.contains_key("api_key") {
                                return true;
                            }
                        }
                    }
                    // Check databricks section
                    if let Some(databricks) = providers_table.get("databricks") {
                        if let Some(databricks_table) = databricks.as_table() {
                            // If databricks has host directly, it's old format
                            if databricks_table.contains_key("host") {
                                return true;
                            }
                        }
                    }
                    // Check openai section
                    if let Some(openai) = providers_table.get("openai") {
                        if let Some(openai_table) = openai.as_table() {
                            // If openai has api_key directly, it's old format
                            if openai_table.contains_key("api_key") {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Validate a provider reference (format: "<provider_type>.<config_name>")
    fn validate_provider_reference(&self, reference: &str) -> Result<()> {
        let parts: Vec<&str> = reference.split('.').collect();
        if parts.len() != 2 {
            anyhow::bail!(
                "Invalid provider reference '{}'. Expected format: '<provider_type>.<config_name>'",
                reference
            );
        }

        let (provider_type, config_name) = (parts[0], parts[1]);

        match provider_type {
            "anthropic" => {
                if !self.providers.anthropic.contains_key(config_name) {
                    anyhow::bail!(
                        "Provider config 'anthropic.{}' not found. Available: {:?}",
                        config_name,
                        self.providers.anthropic.keys().collect::<Vec<_>>()
                    );
                }
            }
            "openai" => {
                if !self.providers.openai.contains_key(config_name) {
                    anyhow::bail!(
                        "Provider config 'openai.{}' not found. Available: {:?}",
                        config_name,
                        self.providers.openai.keys().collect::<Vec<_>>()
                    );
                }
            }
            "databricks" => {
                if !self.providers.databricks.contains_key(config_name) {
                    anyhow::bail!(
                        "Provider config 'databricks.{}' not found. Available: {:?}",
                        config_name,
                        self.providers.databricks.keys().collect::<Vec<_>>()
                    );
                }
            }
            "embedded" => {
                if !self.providers.embedded.contains_key(config_name) {
                    anyhow::bail!(
                        "Provider config 'embedded.{}' not found. Available: {:?}",
                        config_name,
                        self.providers.embedded.keys().collect::<Vec<_>>()
                    );
                }
            }
            "azure" => {
                if !self.providers.azure.contains_key(config_name) {
                    anyhow::bail!(
                        "Provider config 'azure.{}' not found. Available: {:?}",
                        config_name,
                        self.providers.azure.keys().collect::<Vec<_>>()
                    );
                }
            }
            _ => {
                // Check openai_compatible providers
                if !self.providers.openai_compatible.contains_key(provider_type) {
                    anyhow::bail!(
                        "Unknown provider type '{}'. Valid types: anthropic, openai, databricks, embedded, azure, or openai_compatible names",
                        provider_type
                    );
                }
            }
        }

        Ok(())
    }

    /// Parse a provider reference into (provider_type, config_name)
    pub fn parse_provider_reference(reference: &str) -> Result<(String, String)> {
        let parts: Vec<&str> = reference.split('.').collect();
        if parts.len() != 2 {
            anyhow::bail!(
                "Invalid provider reference '{}'. Expected format: '<provider_type>.<config_name>'",
                reference
            );
        }
        Ok((parts[0].to_string(), parts[1].to_string()))
    }

    pub fn save(&self, path: &str) -> Result<()> {
        let toml_string = toml::to_string_pretty(self)?;
        std::fs::write(path, toml_string)?;
        Ok(())
    }

    pub fn load_with_overrides(
        config_path: Option<&str>,
        provider_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<Self> {
        let mut config = Self::load(config_path)?;

        // Apply provider override
        if let Some(provider) = provider_override {
            // Validate the override
            config.validate_provider_reference(&provider)?;
            config.providers.default_provider = provider;
        }

        // Apply model override to the active provider
        if let Some(model) = model_override {
            let (provider_type, config_name) = Self::parse_provider_reference(
                &config.providers.default_provider
            )?;

            match provider_type.as_str() {
                "anthropic" => {
                    if let Some(ref mut anthropic_config) = config.providers.anthropic.get_mut(&config_name) {
                        anthropic_config.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider config 'anthropic.{}' not found.",
                            config_name
                        ));
                    }
                }
                "databricks" => {
                    if let Some(ref mut databricks_config) = config.providers.databricks.get_mut(&config_name) {
                        databricks_config.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider config 'databricks.{}' not found.",
                            config_name
                        ));
                    }
                }
                "embedded" => {
                    if let Some(ref mut embedded_config) = config.providers.embedded.get_mut(&config_name) {
                        embedded_config.model_path = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider config 'embedded.{}' not found.",
                            config_name
                        ));
                    }
                }
                "openai" => {
                    if let Some(ref mut openai_config) = config.providers.openai.get_mut(&config_name) {
                        openai_config.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider config 'openai.{}' not found.",
                            config_name
                        ));
                    }
                }
                "azure" => {
                    if let Some(ref mut azure_config) = config.providers.azure.get_mut(&config_name) {
                        azure_config.deployment = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider config 'azure.{}' not found.",
                            config_name
                        ));
                    }
                }
                _ => {
                    // Check openai_compatible
                    if let Some(ref mut compat_config) = config.providers.openai_compatible.get_mut(&provider_type) {
                        compat_config.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Unknown provider type: {}",
                            provider_type
                        ));
                    }
                }
            }
        }

        Ok(config)
    }

    /// Get the provider reference for planner mode
    pub fn get_planner_provider(&self) -> &str {
        self.providers
            .planner
            .as_deref()
            .unwrap_or(&self.providers.default_provider)
    }

    /// Get the provider reference for coach mode in autonomous execution
    pub fn get_coach_provider(&self) -> &str {
        self.providers
            .coach
            .as_deref()
            .unwrap_or(&self.providers.default_provider)
    }

    /// Get the provider reference for player mode in autonomous execution
    pub fn get_player_provider(&self) -> &str {
        self.providers
            .player
            .as_deref()
            .unwrap_or(&self.providers.default_provider)
    }

    /// Create a copy of the config with a different default provider
    pub fn with_provider_override(&self, provider_ref: &str) -> Result<Self> {
        // Validate that the provider is configured
        self.validate_provider_reference(provider_ref)?;

        let mut config = self.clone();
        config.providers.default_provider = provider_ref.to_string();
        Ok(config)
    }

    /// Create a copy of the config for planner mode
    pub fn for_planner(&self) -> Result<Self> {
        self.with_provider_override(self.get_planner_provider())
    }

    /// Create a copy of the config for coach mode in autonomous execution
    pub fn for_coach(&self) -> Result<Self> {
        self.with_provider_override(self.get_coach_provider())
    }

    /// Create a copy of the config for player mode in autonomous execution
    pub fn for_player(&self) -> Result<Self> {
        self.with_provider_override(self.get_player_provider())
    }

    /// Get Anthropic config by name
    pub fn get_anthropic_config(&self, name: &str) -> Option<&AnthropicConfig> {
        self.providers.anthropic.get(name)
    }

    /// Get OpenAI config by name
    pub fn get_openai_config(&self, name: &str) -> Option<&OpenAIConfig> {
        self.providers.openai.get(name)
    }

    /// Get Databricks config by name
    pub fn get_databricks_config(&self, name: &str) -> Option<&DatabricksConfig> {
        self.providers.databricks.get(name)
    }

    /// Get Embedded config by name
    pub fn get_embedded_config(&self, name: &str) -> Option<&EmbeddedConfig> {
        self.providers.embedded.get(name)
    }

    /// Get Azure config by name
    pub fn get_azure_config(&self, name: &str) -> Option<&AzureConfig> {
        self.providers.azure.get(name)
    }

    /// Get the current default provider's config
    pub fn get_default_provider_config(&self) -> Result<ProviderConfigRef<'_>> {
        let (provider_type, config_name) = Self::parse_provider_reference(
            &self.providers.default_provider
        )?;

        match provider_type.as_str() {
            "anthropic" => {
                self.providers.anthropic.get(&config_name)
                    .map(ProviderConfigRef::Anthropic)
                    .ok_or_else(|| anyhow::anyhow!("Anthropic config '{}' not found", config_name))
            }
            "openai" => {
                self.providers.openai.get(&config_name)
                    .map(ProviderConfigRef::OpenAI)
                    .ok_or_else(|| anyhow::anyhow!("OpenAI config '{}' not found", config_name))
            }
            "databricks" => {
                self.providers.databricks.get(&config_name)
                    .map(ProviderConfigRef::Databricks)
                    .ok_or_else(|| anyhow::anyhow!("Databricks config '{}' not found", config_name))
            }
            "embedded" => {
                self.providers.embedded.get(&config_name)
                    .map(ProviderConfigRef::Embedded)
                    .ok_or_else(|| anyhow::anyhow!("Embedded config '{}' not found", config_name))
            }
            "azure" => {
                self.providers.azure.get(&config_name)
                    .map(ProviderConfigRef::Azure)
                    .ok_or_else(|| anyhow::anyhow!("Azure config '{}' not found", config_name))
            }
            _ => {
                self.providers.openai_compatible.get(&provider_type)
                    .map(ProviderConfigRef::OpenAICompatible)
                    .ok_or_else(|| anyhow::anyhow!("OpenAI compatible config '{}' not found", provider_type))
            }
        }
    }
}

/// Reference to a provider configuration
#[derive(Debug)]
pub enum ProviderConfigRef<'a> {
    Anthropic(&'a AnthropicConfig),
    OpenAI(&'a OpenAIConfig),
    Databricks(&'a DatabricksConfig),
    Embedded(&'a EmbeddedConfig),
    OpenAICompatible(&'a OpenAIConfig),
    Azure(&'a AzureConfig),
}

#[cfg(test)]
mod tests;
