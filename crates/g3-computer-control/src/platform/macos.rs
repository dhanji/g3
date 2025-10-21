use crate::{ComputerController, types::Rect};
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use tesseract::Tesseract;

pub struct MacOSController {
    // Empty struct for now
}

impl MacOSController {
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

#[async_trait]
impl ComputerController for MacOSController {
    async fn take_screenshot(&self, path: &str, region: Option<Rect>, window_id: Option<&str>) -> Result<()> {
        // Determine the temporary directory for screenshots
        let temp_dir = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/tmp", h)))
            .unwrap_or_else(|_| "/tmp".to_string());
        
        // Ensure temp directory exists
        std::fs::create_dir_all(&temp_dir)?;
        
        // If path is relative or doesn't specify a directory, use temp_dir
        let final_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", temp_dir.trim_end_matches('/'), path)
        };
        
        let path_obj = Path::new(&final_path);
        if let Some(parent) = path_obj.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        let mut cmd = std::process::Command::new("screencapture");
        
        // Add flags
        cmd.arg("-x"); // No sound
        
        if let Some(region) = region {
            // Capture specific region: -R x,y,width,height
            cmd.arg("-R");
            cmd.arg(format!("{},{},{},{}", region.x, region.y, region.width, region.height));
        }
        
        if let Some(app_name) = window_id {
            // Capture specific window by app name
            // Use AppleScript to get window ID
            let script = format!(r#"tell application "{}" to id of window 1"#, app_name);
            let output = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()?;
            
            if output.status.success() {
                let window_id_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                cmd.arg(format!("-l{}", window_id_str));
            }
        }
        
        cmd.arg(&final_path);
        
        let screenshot_result = cmd.output()?;
        
        if !screenshot_result.status.success() {
            let stderr = String::from_utf8_lossy(&screenshot_result.stderr);
            return Err(anyhow::anyhow!("screencapture failed: {}", stderr));
        }
        
        Ok(())
    }
    
    async fn extract_text_from_screen(&self, region: Rect) -> Result<String> {
        // Take screenshot of region first
        let temp_path = format!("/tmp/g3_ocr_{}.png", uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, Some(region), None).await?;
        
        // Extract text from the screenshot
        let result = self.extract_text_from_image(&temp_path).await?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        Ok(result)
    }
    
    async fn extract_text_from_image(&self, path: &str) -> Result<String> {
        // Check if tesseract is available on the system
        let tesseract_check = std::process::Command::new("which")
            .arg("tesseract")
            .output();
        
        if tesseract_check.is_err() || !tesseract_check.as_ref().unwrap().status.success() {
            anyhow::bail!("Tesseract OCR is not installed on your system.\n\n\
                To install tesseract:\n  macOS:   brew install tesseract\n  \
                Linux:   sudo apt-get install tesseract-ocr (Ubuntu/Debian)\n           \
                sudo yum install tesseract (RHEL/CentOS)\n  \
                Windows: Download from https://github.com/UB-Mannheim/tesseract/wiki\n\n\
                After installation, restart your terminal and try again.");
        }
        
        // Initialize Tesseract
        let tess = Tesseract::new(None, Some("eng"))
            .map_err(|e| {
                anyhow::anyhow!("Failed to initialize Tesseract: {}\n\n\
                    This usually means:\n1. Tesseract is not properly installed\n\
                    2. Language data files are missing\n\nTo fix:\n  \
                    macOS:   brew reinstall tesseract\n  \
                    Linux:   sudo apt-get install tesseract-ocr-eng\n  \
                    Windows: Reinstall tesseract and ensure language files are included", e)
            })?;
        
        let text = tess.set_image(path)
            .map_err(|e| anyhow::anyhow!("Failed to load image '{}': {}", path, e))?
            .get_text()
            .map_err(|e| anyhow::anyhow!("Failed to extract text from image: {}", e))?;
        
        Ok(text)
    }
}