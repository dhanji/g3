use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Represents a G3 project with workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// The workspace directory for the project
    pub workspace_dir: PathBuf,
    
    /// Path to the requirements document (for autonomous mode)
    pub requirements_path: Option<PathBuf>,
    
    /// Whether the project is in autonomous mode
    pub autonomous: bool,
    
    /// Project name (derived from workspace directory name)
    pub name: String,
    
    /// Session ID for tracking
    pub session_id: Option<String>,
}

impl Project {
    /// Create a new project with the given workspace directory
    pub fn new(workspace_dir: PathBuf) -> Self {
        let name = workspace_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        
        Self {
            workspace_dir,
            requirements_path: None,
            autonomous: false,
            name,
            session_id: None,
        }
    }
    
    /// Create a project for autonomous mode
    pub fn new_autonomous(workspace_dir: PathBuf) -> Result<Self> {
        let mut project = Self::new(workspace_dir.clone());
        project.autonomous = true;
        
        // Look for requirements.md in the workspace directory
        let requirements_path = workspace_dir.join("requirements.md");
        if requirements_path.exists() {
            project.requirements_path = Some(requirements_path);
        }
        
        Ok(project)
    }
    
    /// Set the workspace directory and update related paths
    pub fn set_workspace(&mut self, workspace_dir: PathBuf) {
        self.workspace_dir = workspace_dir.clone();
        self.name = workspace_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();
        
        // Update requirements path if in autonomous mode
        if self.autonomous {
            let requirements_path = workspace_dir.join("requirements.md");
            if requirements_path.exists() {
                self.requirements_path = Some(requirements_path);
            }
        }
    }
    
    /// Get the workspace directory
    pub fn workspace(&self) -> &Path {
        &self.workspace_dir
    }
    
    /// Check if requirements file exists
    pub fn has_requirements(&self) -> bool {
        self.requirements_path.is_some()
    }
    
    /// Read the requirements file content
    pub fn read_requirements(&self) -> Result<Option<String>> {
        if let Some(ref path) = self.requirements_path {
            Ok(Some(std::fs::read_to_string(path)?))
        } else {
            Ok(None)
        }
    }
    
    /// Create the workspace directory if it doesn't exist
    pub fn ensure_workspace_exists(&self) -> Result<()> {
        if !self.workspace_dir.exists() {
            std::fs::create_dir_all(&self.workspace_dir)?;
        }
        Ok(())
    }
    
    /// Change to the workspace directory
    pub fn enter_workspace(&self) -> Result<()> {
        std::env::set_current_dir(&self.workspace_dir)?;
        Ok(())
    }
    
    /// Get the logs directory for the project
    pub fn logs_dir(&self) -> PathBuf {
        self.workspace_dir.join("logs")
    }
    
    /// Ensure the logs directory exists
    pub fn ensure_logs_dir(&self) -> Result<()> {
        let logs_dir = self.logs_dir();
        if !logs_dir.exists() {
            std::fs::create_dir_all(&logs_dir)?;
        }
        Ok(())
    }
}