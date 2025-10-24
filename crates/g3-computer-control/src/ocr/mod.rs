use crate::types::TextLocation;
use anyhow::Result;
use async_trait::async_trait;

/// OCR engine trait for text recognition with bounding boxes
#[async_trait]
pub trait OCREngine: Send + Sync {
    /// Extract text with locations from an image file
    async fn extract_text_with_locations(&self, path: &str) -> Result<Vec<TextLocation>>;
    
    /// Get the name of the OCR engine
    fn name(&self) -> &str;
}

// Platform-specific modules
#[cfg(target_os = "macos")]
pub mod vision;

pub mod tesseract;

// Re-export the default OCR engine for the platform
#[cfg(target_os = "macos")]
pub use vision::AppleVisionOCR as DefaultOCR;

#[cfg(not(target_os = "macos"))]
pub use tesseract::TesseractOCR as DefaultOCR;
