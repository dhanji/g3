use serde::{Deserialize, Serialize};
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub providers: ProvidersConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub openai: Option<OpenAIConfig>,
    pub anthropic: Option<AnthropicConfig>,
    pub embedded: Option<EmbeddedConfig>,
    pub default_provider: String,
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
    pub max_context_length: usize,
    pub enable_streaming: bool,
    pub timeout_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: ProvidersConfig {
                openai: None,
                anthropic: None,
                embedded: None,
                default_provider: "anthropic".to_string(),
            },
            agent: AgentConfig {
                max_context_length: 8192,
                enable_streaming: true,
                timeout_seconds: 60,
            },
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
        
        // If no config exists, create and save a default Qwen config
        if !config_exists {
            let qwen_config = Self::default_qwen_config();
            
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
            if let Err(e) = qwen_config.save(config_file.to_str().unwrap()) {
                eprintln!("Warning: Could not save default config: {}", e);
            } else {
                println!("Created default Qwen configuration at: {}", config_file.display());
            }
            
            return Ok(qwen_config);
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
    
    fn default_qwen_config() -> Self {
        Self {
            providers: ProvidersConfig {
                openai: None,
                anthropic: None,
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
            },
            agent: AgentConfig {
                max_context_length: 8192,
                enable_streaming: true,
                timeout_seconds: 60,
            },
        }
    }
    
    pub fn save(&self, path: &str) -> Result<()> {
        let toml_string = toml::to_string_pretty(self)?;
        std::fs::write(path, toml_string)?;
        Ok(())
    }
}
