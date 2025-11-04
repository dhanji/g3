use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleState {
    pub theme: String,
    pub last_workspace: Option<String>,
    pub g3_binary_path: Option<String>,
    pub last_provider: Option<String>,
    pub last_model: Option<String>,
}

impl Default for ConsoleState {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            last_workspace: None,
            g3_binary_path: None,
            last_provider: Some("databricks".to_string()),
            last_model: Some("databricks-claude-sonnet-4-5".to_string()),
        }
    }
}

impl ConsoleState {
    pub fn load() -> Self {
        let config_path = Self::config_path();
        
        if config_path.exists() {
            if let Ok(content) = fs::read_to_string(&config_path) {
                return serde_json::from_str(&content).unwrap_or_else(|e| {
                    tracing::warn!("Failed to parse console state: {}", e);
                    Self::default()
                });
            }
        }
        
        Self::default()
    }
    
    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::config_path();
        info!("Saving console state to: {:?}", config_path);
        
        // Create parent directory if it doesn't exist
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&config_path, content)?;
        info!("Console state saved successfully to: {:?}", config_path);
        
        Ok(())
    }
    
    fn config_path() -> PathBuf {
        // Use explicit ~/.config/g3/console-state.json path as per requirements
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".config")
            .join("g3")
            .join("console-state.json")
    }
}
