use super::OCREngine;
use crate::types::TextLocation;
use anyhow::{Result, Context};
use async_trait::async_trait;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_float, c_uint};

// FFI bindings to Swift VisionBridge
#[repr(C)]
struct VisionTextBox {
    text: *const c_char,
    text_len: c_uint,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    confidence: c_float,
}

extern "C" {
    fn vision_recognize_text(
        image_path: *const c_char,
        image_path_len: c_uint,
        out_boxes: *mut *mut std::ffi::c_void,
        out_count: *mut c_uint,
    ) -> bool;
    
    fn vision_free_boxes(boxes: *mut std::ffi::c_void, count: c_uint);
}

/// Apple Vision Framework OCR engine
pub struct AppleVisionOCR;

impl AppleVisionOCR {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[async_trait]
impl OCREngine for AppleVisionOCR {
    async fn extract_text_with_locations(&self, path: &str) -> Result<Vec<TextLocation>> {
        // Convert path to C string
        let c_path = CString::new(path)
            .context("Failed to convert path to C string")?;
        
        let mut boxes_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut count: c_uint = 0;
        
        // Call Swift Vision API
        let success = unsafe {
            vision_recognize_text(
                c_path.as_ptr(),
                path.len() as c_uint,
                &mut boxes_ptr,
                &mut count,
            )
        };
        
        if !success || boxes_ptr.is_null() {
            anyhow::bail!("Apple Vision OCR failed");
        }
        
        // Convert C array to Rust Vec
        let mut locations = Vec::new();
        
        unsafe {
            let typed_boxes = boxes_ptr as *const VisionTextBox;
            let boxes_slice = std::slice::from_raw_parts(typed_boxes, count as usize);
            
            for box_data in boxes_slice {
                // Convert C string to Rust String
                let text = if !box_data.text.is_null() {
                    CStr::from_ptr(box_data.text)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::new()
                };
                
                if !text.is_empty() {
                    locations.push(TextLocation {
                        text,
                        x: box_data.x,
                        y: box_data.y,
                        width: box_data.width,
                        height: box_data.height,
                        confidence: box_data.confidence,
                    });
                }
            }
            
            // Free the C array
            vision_free_boxes(boxes_ptr, count);
        }
        
        Ok(locations)
    }
    
    fn name(&self) -> &str {
        "Apple Vision Framework"
    }
}
