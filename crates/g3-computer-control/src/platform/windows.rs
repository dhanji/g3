use crate::{types::*, ComputerController};
use anyhow::Result;
use async_trait::async_trait;

pub struct WindowsController {
    // Placeholder for Windows-specific state
}

impl WindowsController {
    pub fn new() -> Result<Self> {
        tracing::warn!("Windows computer control not fully implemented");
        Ok(Self {})
    }
}

#[async_trait]
impl ComputerController for WindowsController {
    async fn take_screenshot(
        &self,
        _path: &str,
        _region: Option<Rect>,
        _window_id: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!("Windows screenshot implementation not yet available")
    }

    async fn extract_text_from_screen(&self, _region: Rect, _window_id: &str) -> Result<String> {
        anyhow::bail!("Windows OCR implementation not yet available")
    }

    async fn extract_text_from_image(&self, _path: &str) -> Result<String> {
        anyhow::bail!("Windows OCR implementation not yet available")
    }

    async fn extract_text_with_locations(&self, _path: &str) -> Result<Vec<TextLocation>> {
        anyhow::bail!("Windows OCR implementation not yet available")
    }

    async fn find_text_in_app(
        &self,
        _app_name: &str,
        _search_text: &str,
    ) -> Result<Option<TextLocation>> {
        anyhow::bail!("Windows OCR implementation not yet available")
    }

    fn move_mouse(&self, _x: i32, _y: i32) -> Result<()> {
        anyhow::bail!("Windows mouse control implementation not yet available")
    }

    fn click_at(&self, _x: i32, _y: i32, _app_name: Option<&str>) -> Result<()> {
        anyhow::bail!("Windows mouse control implementation not yet available")
    }
}
