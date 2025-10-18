pub mod types;
pub mod platform;

use anyhow::Result;
use async_trait::async_trait;
use types::*;

#[async_trait]
pub trait ComputerController: Send + Sync {
    // Mouse operations
    async fn move_mouse(&self, x: i32, y: i32) -> Result<()>;
    async fn click(&self, button: MouseButton) -> Result<()>;
    async fn double_click(&self, button: MouseButton) -> Result<()>;
    
    // Keyboard operations
    async fn type_text(&self, text: &str) -> Result<()>;
    async fn press_key(&self, key: &str) -> Result<()>;
    
    // Window management
    async fn list_windows(&self) -> Result<Vec<Window>>;
    async fn focus_window(&self, window_id: &str) -> Result<()>;
    async fn get_window_bounds(&self, window_id: &str) -> Result<Rect>;
    
    // UI element inspection
    async fn find_element(&self, selector: &ElementSelector) -> Result<Option<UIElement>>;
    async fn get_element_text(&self, element_id: &str) -> Result<String>;
    async fn get_element_bounds(&self, element_id: &str) -> Result<Rect>;
    
    // Screen capture
    async fn take_screenshot(&self, path: &str, region: Option<Rect>, window_id: Option<&str>) -> Result<()>;
    
    // OCR operations
    async fn extract_text_from_screen(&self, region: Rect) -> Result<OCRResult>;
    async fn extract_text_from_image(&self, path: &str) -> Result<OCRResult>;
    async fn find_text_on_screen(&self, text: &str) -> Result<Option<Point>>;
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
