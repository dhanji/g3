use crate::{types::*, ComputerController};
use anyhow::Result;
use async_trait::async_trait;

pub struct LinuxController {
    // Placeholder for X11 connection or other state
}

impl LinuxController {
    pub fn new() -> Result<Self> {
        // Initialize X11 connection
        tracing::warn!("Linux computer control not fully implemented");
        Ok(Self {})
    }
}

#[async_trait]
impl ComputerController for LinuxController {
    async fn take_screenshot(
        &self,
        _path: &str,
        _region: Option<Rect>,
        _window_id: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!("Linux screenshot not yet implemented")
    }

    async fn extract_text_from_screen(&self, _region: Rect, _window_id: &str) -> Result<String> {
        anyhow::bail!("Linux implementation not yet available")
    }

    async fn extract_text_from_image(&self, _path: &str) -> Result<String> {
        anyhow::bail!("Linux OCR not yet implemented")
    }

    async fn extract_text_with_locations(&self, _path: &str) -> Result<Vec<TextLocation>> {
        anyhow::bail!("Linux OCR with locations not yet implemented")
    }

    async fn find_text_in_app(
        &self,
        _app_name: &str,
        _search_text: &str,
    ) -> Result<Option<TextLocation>> {
        anyhow::bail!("Linux find_text_in_app not yet implemented")
    }

    fn move_mouse(&self, _x: i32, _y: i32) -> Result<()> {
        anyhow::bail!("Linux mouse control not yet implemented")
    }

    fn click_at(&self, _x: i32, _y: i32, _app_name: Option<&str>) -> Result<()> {
        anyhow::bail!("Linux click not yet implemented")
    }
}
