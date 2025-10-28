pub mod controller;

pub use controller::MacAxController;

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// Represents an accessibility element in the UI hierarchy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AXElement {
    pub role: String,
    pub title: Option<String>,
    pub value: Option<String>,
    pub label: Option<String>,
    pub identifier: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub position: Option<(f64, f64)>,
    pub size: Option<(f64, f64)>,
    pub children_count: usize,
}

/// Represents a macOS application
#[derive(Debug, Clone)]
pub struct AXApplication {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
}

impl AXElement {
    /// Convert to a human-readable string representation
    pub fn to_string(&self) -> String {
        let mut parts = vec![format!("Role: {}", self.role)];
        
        if let Some(ref title) = self.title {
            parts.push(format!("Title: {}", title));
        }
        if let Some(ref value) = self.value {
            parts.push(format!("Value: {}", value));
        }
        if let Some(ref label) = self.label {
            parts.push(format!("Label: {}", label));
        }
        if let Some(ref id) = self.identifier {
            parts.push(format!("ID: {}", id));
        }
        
        parts.push(format!("Enabled: {}", self.enabled));
        parts.push(format!("Focused: {}", self.focused));
        
        if let Some((x, y)) = self.position {
            parts.push(format!("Position: ({:.0}, {:.0})", x, y));
        }
        if let Some((w, h)) = self.size {
            parts.push(format!("Size: ({:.0}, {:.0})", w, h));
        }
        
        parts.push(format!("Children: {}", self.children_count));
        
        parts.join(", ")
    }
}
