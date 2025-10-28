use crate::{ComputerController, types::*};
use anyhow::Result;
use async_trait::async_trait;
use tesseract::Tesseract;
use uuid::Uuid;

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
    async fn move_mouse(&self, _x: i32, _y: i32) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn click(&self, _button: MouseButton) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn double_click(&self, _button: MouseButton) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn type_text(&self, _text: &str) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn press_key(&self, _key: &str) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn list_windows(&self) -> Result<Vec<Window>> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn focus_window(&self, _window_id: &str) -> Result<()> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn get_window_bounds(&self, _window_id: &str) -> Result<Rect> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn find_element(&self, _selector: &ElementSelector) -> Result<Option<UIElement>> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn get_element_text(&self, _element_id: &str) -> Result<String> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn get_element_bounds(&self, _element_id: &str) -> Result<Rect> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn take_screenshot(&self, _path: &str, _region: Option<Rect>, _window_id: Option<&str>) -> Result<()> {
        // Enforce that window_id must be provided
        if _window_id.is_none() {
            anyhow::bail!("window_id is required. You must specify which window to capture (e.g., 'Firefox', 'Terminal', 'gedit'). Use list_windows to see available windows.");
        }

        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn extract_text_from_screen(&self, _region: Rect, _window_id: &str) -> Result<String> {
        anyhow::bail!("Linux implementation not yet available")
    }
    
    async fn extract_text_from_image(&self, _path: &str) -> Result<OCRResult> {
        // Check if tesseract is available on the system
        let tesseract_check = std::process::Command::new("which")
            .arg("tesseract")
            .output();
        
        if tesseract_check.is_err() || !tesseract_check.as_ref().unwrap().status.success() {
            anyhow::bail!("Tesseract OCR is not installed on your system.\n\n\
                To install tesseract:\n  \
                Ubuntu/Debian: sudo apt-get install tesseract-ocr\n  \
                RHEL/CentOS:   sudo yum install tesseract\n  \
                Arch Linux:    sudo pacman -S tesseract\n\n\
                After installation, restart your terminal and try again.");
        }
        
        // Initialize Tesseract
        let tess = Tesseract::new(None, Some("eng"))
            .map_err(|e| {
                anyhow::anyhow!("Failed to initialize Tesseract: {}\n\n\
                    This usually means:\n1. Tesseract is not properly installed\n\
                    2. Language data files are missing\n\nTo fix:\n  \
                    Ubuntu/Debian: sudo apt-get install tesseract-ocr-eng\n  \
                    RHEL/CentOS:   sudo yum install tesseract-langpack-eng\n  \
                    Arch Linux:    sudo pacman -S tesseract-data-eng", e)
            })?;
        
        let text = tess.set_image(_path)
            .map_err(|e| anyhow::anyhow!("Failed to load image '{}': {}", _path, e))?
            .get_text()
            .map_err(|e| anyhow::anyhow!("Failed to extract text from image: {}", e))?;
        
        // Get confidence (simplified - would need more complex API calls for per-word confidence)
        let confidence = 0.85; // Placeholder
        
        Ok(OCRResult {
            text,
            confidence,
            bounds: Rect { x: 0, y: 0, width: 0, height: 0 }, // Would need image dimensions
        })
    }
    
    async fn find_text_on_screen(&self, _text: &str) -> Result<Option<Point>> {
        // Check if tesseract is available on the system
        let tesseract_check = std::process::Command::new("which")
            .arg("tesseract")
            .output();
        
        if tesseract_check.is_err() || !tesseract_check.as_ref().unwrap().status.success() {
            anyhow::bail!("Tesseract OCR is not installed on your system.\n\n\
                To install tesseract:\n  \
                Ubuntu/Debian: sudo apt-get install tesseract-ocr\n  \
                RHEL/CentOS:   sudo yum install tesseract\n  \
                Arch Linux:    sudo pacman -S tesseract\n\n\
                After installation, restart your terminal and try again.");
        }
        
        // Take full screen screenshot
        let temp_path = format!("/tmp/g3_ocr_search_{}.png", uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, None, None).await?;
        
        // Use Tesseract to find text with bounding boxes
        let tess = Tesseract::new(None, Some("eng"))
            .map_err(|e| {
                anyhow::anyhow!("Failed to initialize Tesseract: {}\n\n\
                    This usually means:\n1. Tesseract is not properly installed\n\
                    2. Language data files are missing\n\nTo fix:\n  \
                    Ubuntu/Debian: sudo apt-get install tesseract-ocr-eng\n  \
                    RHEL/CentOS:   sudo yum install tesseract-langpack-eng\n  \
                    Arch Linux:    sudo pacman -S tesseract-data-eng", e)
            })?;
        
        let full_text = tess.set_image(temp_path.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to load screenshot: {}", e))?
            .get_text()
            .map_err(|e| anyhow::anyhow!("Failed to extract text from screen: {}", e))?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        // Simple text search - full implementation would use get_component_images
        // to get bounding boxes for each word
        if full_text.contains(_text) {
            tracing::warn!("Text found but precise coordinates not available in simplified implementation");
            Ok(Some(Point { x: 0, y: 0 }))
        } else {
            Ok(None)
        }
    }
}
