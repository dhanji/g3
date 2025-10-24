// Suppress warnings from objc crate macros
#![allow(unexpected_cfgs)]

pub mod types;
pub mod platform;
pub mod ocr;
pub mod webdriver;
pub mod macax;

// Re-export webdriver types for convenience
pub use webdriver::{WebDriverController, WebElement, safari::SafariDriver};

// Re-export macax types for convenience
pub use macax::{MacAxController, AXElement, AXApplication};

use anyhow::Result;
use async_trait::async_trait;
use types::*;

#[async_trait]
pub trait ComputerController: Send + Sync {
    // Screen capture
    async fn take_screenshot(&self, path: &str, region: Option<Rect>, window_id: Option<&str>) -> Result<()>;
    
    // OCR operations
    async fn extract_text_from_screen(&self, region: Rect) -> Result<String>;
    async fn extract_text_from_image(&self, path: &str) -> Result<String>;
    async fn extract_text_with_locations(&self, path: &str) -> Result<Vec<TextLocation>>;
    async fn find_text_in_app(&self, app_name: &str, search_text: &str) -> Result<Option<TextLocation>>;
    
    // Mouse operations
    fn move_mouse(&self, x: i32, y: i32) -> Result<()>;
    fn click_at(&self, x: i32, y: i32, app_name: Option<&str>) -> Result<()>;
}

// Platform-specific constructor
pub fn create_controller() -> Result<Box<dyn ComputerController>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(platform::macos::MacOSController::new()?));
    
    #[cfg(target_os = "linux")]
    return Ok(Box::new(platform::linux::LinuxController::new()?));
    
    #[cfg(target_os = "windows")]
    return Ok(Box::new(platform::windows::WindowsController::new()?));
    
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("Unsupported platform")
}
