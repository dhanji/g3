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
    
    /// Override requirements text (takes precedence over requirements_path)
    pub requirements_text: Option<String>,
    
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
            requirements_text: None,
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
    
    /// Create a project for autonomous mode with requirements text override
    pub fn new_autonomous_with_requirements(workspace_dir: PathBuf, requirements_text: String) -> Result<Self> {
        let mut project = Self::new(workspace_dir.clone());
        project.autonomous = true;
        project.requirements_text = Some(requirements_text);
        
        // Don't look for requirements.md file when text is provided
        // The text override takes precedence
        
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
        // Has requirements if either text override is provided or requirements file exists
        self.requirements_text.is_some() || self.requirements_path.is_some()
    }
    
    /// Check if implementation files exist in the workspace
    pub fn has_implementation_files(&self) -> bool {
        self.check_dir_for_implementation_files(&self.workspace_dir)
    }
    
    /// Recursively check a directory for implementation files
    fn check_dir_for_implementation_files(&self, dir: &Path) -> bool {
        // Common source file extensions
        let extensions = vec![
            "swift", "rs", "py", "js", "ts", "java", "cpp", "c",
            "go", "rb", "php", "cs", "kt", "scala", "m", "h"
        ];
        
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                
                if path.is_file() {
                    // Check if it's a source file
                    if let Some(ext) = path.extension() {
                        if let Some(ext_str) = ext.to_str() {
                            if extensions.contains(&ext_str) {
                                return true;
                            }
                        }
                    }
                } else if path.is_dir() {
                    // Skip hidden directories and common non-source directories
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if !name.starts_with('.') && name != "logs" && name != "target" && name != "node_modules" {
                            // Recursively check subdirectories
                            if self.check_dir_for_implementation_files(&path) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }
    
    /// Read the requirements file content
    pub fn read_requirements(&self) -> Result<Option<String>> {
        // Prioritize requirements text override
        if let Some(ref text) = self.requirements_text {
            Ok(Some(text.clone()))
        } else if let Some(ref path) = self.requirements_path {
            // Fall back to reading from file
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