use serde::{Deserialize, Serialize};
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub providers: ProvidersConfig,
    pub agent: AgentConfig,
    pub computer_control: ComputerControlConfig,
    pub webdriver: WebDriverConfig,
    pub macax: MacAxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub openai: Option<OpenAIConfig>,
    pub anthropic: Option<AnthropicConfig>,
    pub databricks: Option<DatabricksConfig>,
    pub embedded: Option<EmbeddedConfig>,
    pub default_provider: String,
    pub coach: Option<String>,  // Provider to use for coach in autonomous mode
    pub player: Option<String>, // Provider to use for player in autonomous mode
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabricksConfig {
    pub host: String,
    pub token: Option<String>, // Optional - will use OAuth if not provided
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub use_oauth: Option<bool>, // Default to true if token not provided
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedConfig {
    pub model_path: String,
    pub model_type: String, // e.g., "llama", "mistral", "codellama"
    pub context_length: Option<u32>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub gpu_layers: Option<u32>, // Number of layers to offload to GPU
    pub threads: Option<u32>,    // Number of CPU threads to use
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub fallback_default_max_tokens: usize,
    pub enable_streaming: bool,
    pub timeout_seconds: u64,
    pub auto_compact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputerControlConfig {
    pub enabled: bool,
    pub require_confirmation: bool,
    pub max_actions_per_second: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDriverConfig {
    pub enabled: bool,
    pub safari_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacAxConfig {
    pub enabled: bool,
}

impl Default for MacAxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
        }
    }
}

impl Default for WebDriverConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            safari_port: 4444,
        }
    }
}

impl Default for ComputerControlConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for safety
            require_confirmation: true,
            max_actions_per_second: 5,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: ProvidersConfig {
                openai: None,
                anthropic: None,
                databricks: Some(DatabricksConfig {
                    host: "https://your-workspace.cloud.databricks.com".to_string(),
                    token: None, // Will use OAuth by default
                    model: "databricks-claude-sonnet-4".to_string(),
                    max_tokens: Some(4096),
                    temperature: Some(0.1),
                    use_oauth: Some(true),
                }),
                embedded: None,
                default_provider: "databricks".to_string(),
                coach: None,  // Will use default_provider if not specified
                player: None, // Will use default_provider if not specified
            },
            agent: AgentConfig {
                fallback_default_max_tokens: 8192,
                enable_streaming: true,
                timeout_seconds: 60,
                auto_compact: true,
            },
            computer_control: ComputerControlConfig::default(),
            webdriver: WebDriverConfig::default(),
            macax: MacAxConfig::default(),
        }
    }
}

impl Config {
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        // Check if any config file exists
        let config_exists = if let Some(path) = config_path {
            Path::new(path).exists()
        } else {
            // Check default locations
            let default_paths = [
                "./g3.toml",
                "~/.config/g3/config.toml",
                "~/.g3.toml",
            ];
            
            default_paths.iter().any(|path| {
                let expanded_path = shellexpand::tilde(path);
                Path::new(expanded_path.as_ref()).exists()
            })
        };
        
        // If no config exists, create and save a default Databricks config
        if !config_exists {
            let databricks_config = Self::default();
            
            // Save to default location
            let config_dir = dirs::home_dir()
                .map(|mut path| {
                    path.push(".config");
                    path.push("g3");
                    path
                })
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            
            // Create directory if it doesn't exist
            std::fs::create_dir_all(&config_dir).ok();
            
            let config_file = config_dir.join("config.toml");
            if let Err(e) = databricks_config.save(config_file.to_str().unwrap()) {
                eprintln!("Warning: Could not save default config: {}", e);
            } else {
                println!("Created default Databricks configuration at: {}", config_file.display());
            }
            
            return Ok(databricks_config);
        }
        
        // Existing config loading logic
        let mut settings = config::Config::builder();
        
        // Load default configuration
        settings = settings.add_source(config::Config::try_from(&Config::default())?);
        
        // Load from config file if provided
        if let Some(path) = config_path {
            if Path::new(path).exists() {
                settings = settings.add_source(config::File::with_name(path));
            }
        } else {
            // Try to load from default locations
            let default_paths = [
                "./g3.toml",
                "~/.config/g3/config.toml",
                "~/.g3.toml",
            ];
            
            for path in &default_paths {
                let expanded_path = shellexpand::tilde(path);
                if Path::new(expanded_path.as_ref()).exists() {
                    settings = settings.add_source(config::File::with_name(expanded_path.as_ref()));
                    break;
                }
            }
        }
        
        // Override with environment variables
        settings = settings.add_source(
            config::Environment::with_prefix("G3")
                .separator("_")
        );
        
        let config = settings.build()?.try_deserialize()?;
        Ok(config)
    }

    #[allow(dead_code)]
    fn default_qwen_config() -> Self {
        Self {
            providers: ProvidersConfig {
                openai: None,
                anthropic: None,
                databricks: None,
                embedded: Some(EmbeddedConfig {
                    model_path: "~/.cache/g3/models/qwen2.5-7b-instruct-q3_k_m.gguf".to_string(),
                    model_type: "qwen".to_string(),
                    context_length: Some(32768),  // Qwen2.5 supports 32k context
                    max_tokens: Some(2048),
                    temperature: Some(0.1),
                    gpu_layers: Some(32),
                    threads: Some(8),
                }),
                default_provider: "embedded".to_string(),
                coach: None,  // Will use default_provider if not specified
                player: None, // Will use default_provider if not specified
            },
            agent: AgentConfig {
                fallback_default_max_tokens: 8192,
                enable_streaming: true,
                timeout_seconds: 60,
                auto_compact: true,
            },
            computer_control: ComputerControlConfig::default(),
            webdriver: WebDriverConfig::default(),
            macax: MacAxConfig::default(),
        }
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
        // Load the base configuration
        let mut config = Self::load(config_path)?;
        
        // Apply provider override
        if let Some(provider) = provider_override {
            config.providers.default_provider = provider;
        }
        
        // Apply model override to the active provider
        if let Some(model) = model_override {
            match config.providers.default_provider.as_str() {
                "anthropic" => {
                    if let Some(ref mut anthropic) = config.providers.anthropic {
                        anthropic.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider 'anthropic' is not configured. Please add anthropic configuration to your config file."
                        ));
                    }
                }
                "databricks" => {
                    if let Some(ref mut databricks) = config.providers.databricks {
                        databricks.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider 'databricks' is not configured. Please add databricks configuration to your config file."
                        ));
                    }
                }
                "embedded" => {
                    if let Some(ref mut embedded) = config.providers.embedded {
                        embedded.model_path = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider 'embedded' is not configured. Please add embedded configuration to your config file."
                        ));
                    }
                }
                "openai" => {
                    if let Some(ref mut openai) = config.providers.openai {
                        openai.model = model;
                    } else {
                        return Err(anyhow::anyhow!(
                            "Provider 'openai' is not configured. Please add openai configuration to your config file."
                        ));
                    }
                }
                _ => return Err(anyhow::anyhow!("Unknown provider: {}", 
                    config.providers.default_provider)),
            }
        }
        
        Ok(config)
    }
    
    /// Get the provider to use for coach mode in autonomous execution
    pub fn get_coach_provider(&self) -> &str {
        self.providers.coach
            .as_deref()
            .unwrap_or(&self.providers.default_provider)
    }
    
    /// Get the provider to use for player mode in autonomous execution
    pub fn get_player_provider(&self) -> &str {
        self.providers.player
            .as_deref()
            .unwrap_or(&self.providers.default_provider)
    }
    
    /// Create a copy of the config with a different default provider
    pub fn with_provider_override(&self, provider: &str) -> Result<Self> {
        // Validate that the provider is configured
        match provider {
            "anthropic" if self.providers.anthropic.is_none() => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is specified but not configured. Please add {} configuration to your config file.",
                    provider, provider
                ));
            }
            "databricks" if self.providers.databricks.is_none() => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is specified but not configured. Please add {} configuration to your config file.",
                    provider, provider
                ));
            }
            "embedded" if self.providers.embedded.is_none() => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is specified but not configured. Please add {} configuration to your config file.",
                    provider, provider
                ));
            }
            "openai" if self.providers.openai.is_none() => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is specified but not configured. Please add {} configuration to your config file.",
                    provider, provider
                ));
            }
            _ => {} // Provider is configured or unknown (will be caught later)
        }
        
        let mut config = self.clone();
        config.providers.default_provider = provider.to_string();
        Ok(config)
    }
    
    /// Create a copy of the config for coach mode in autonomous execution
    pub fn for_coach(&self) -> Result<Self> {
        self.with_provider_override(self.get_coach_provider())
    }
    
    /// Create a copy of the config for player mode in autonomous execution
    pub fn for_player(&self) -> Result<Self> {
        self.with_provider_override(self.get_player_provider())
    }
}

#[cfg(test)]
mod tests;
