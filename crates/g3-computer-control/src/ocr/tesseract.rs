use super::OCREngine;
use crate::types::TextLocation;
use anyhow::Result;
use async_trait::async_trait;

/// Tesseract OCR engine (fallback/cross-platform)
pub struct TesseractOCR;

impl TesseractOCR {
    pub fn new() -> Result<Self> {
        // Check if tesseract is available
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
        
        Ok(Self)
    }
}

#[async_trait]
impl OCREngine for TesseractOCR {
    async fn extract_text_with_locations(&self, path: &str) -> Result<Vec<TextLocation>> {
        // Use tesseract CLI with TSV output to get bounding boxes
        let output = std::process::Command::new("tesseract")
            .arg(path)
            .arg("stdout")
            .arg("tsv")
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run tesseract: {}", e))?;
        
        if !output.status.success() {
            anyhow::bail!("Tesseract failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        
        let tsv_text = String::from_utf8_lossy(&output.stdout);
        let mut locations = Vec::new();
        
        // Parse TSV output (skip header line)
        for (i, line) in tsv_text.lines().enumerate() {
            if i == 0 { continue; } // Skip header
            
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 12 {
                // TSV format: level, page_num, block_num, par_num, line_num, word_num,
                //             left, top, width, height, conf, text
                if let (Ok(x), Ok(y), Ok(w), Ok(h), Ok(conf), text) = (
                    parts[6].parse::<i32>(),
                    parts[7].parse::<i32>(),
                    parts[8].parse::<i32>(),
                    parts[9].parse::<i32>(),
                    parts[10].parse::<f32>(),
                    parts[11],
                ) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && conf > 0.0 {
                        locations.push(TextLocation {
                            text: trimmed.to_string(),
                            x,
                            y,
                            width: w,
                            height: h,
                            confidence: conf / 100.0, // Convert from 0-100 to 0-1
                        });
                    }
                }
            }
        }
        
        Ok(locations)
    }
    
    fn name(&self) -> &str {
        "Tesseract OCR"
    }
}
