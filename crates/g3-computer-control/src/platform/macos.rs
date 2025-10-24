use crate::{ComputerController, types::{Rect, TextLocation}};
use crate::ocr::{OCREngine, DefaultOCR};
use anyhow::{Result, Context};
use async_trait::async_trait;
use std::path::Path;
use core_graphics::window::{kCGWindowListOptionOnScreenOnly, kCGNullWindowID, CGWindowListCopyWindowInfo};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation::base::{TCFType, ToVoid};
use core_foundation::array::CFArray;

pub struct MacOSController {
    ocr_engine: Box<dyn OCREngine>,
    #[allow(dead_code)]
    ocr_name: String,
}

impl MacOSController {
    pub fn new() -> Result<Self> {
        let ocr = Box::new(DefaultOCR::new()?);
        let ocr_name = ocr.name().to_string();
        tracing::info!("Initialized macOS controller with OCR engine: {}", ocr_name);
        Ok(Self { ocr_engine: ocr, ocr_name })
    }
}

#[async_trait]
impl ComputerController for MacOSController {
    async fn take_screenshot(&self, path: &str, region: Option<Rect>, window_id: Option<&str>) -> Result<()> {
        // Enforce that window_id must be provided
        if window_id.is_none() {
            return Err(anyhow::anyhow!("window_id is required. You must specify which window to capture (e.g., 'Safari', 'Terminal', 'Google Chrome'). Use list_windows to see available windows."));
        }

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
        
        let app_name = window_id.unwrap(); // Safe because we checked is_none() above
        
        // Get the window ID for the specified application
        let cg_window_id = unsafe {
            let window_list = CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly,
                kCGNullWindowID
            );
            
            let array = CFArray::<CFDictionary>::wrap_under_create_rule(window_list);
            let count = array.len();
            
            let mut found_window_id: Option<(u32, String, bool)> = None; // (id, owner, is_exact_match)
            let app_name_lower = app_name.to_lowercase();
            
            for i in 0..count {
                let dict = array.get(i).unwrap();
                
                // Get owner name
                let owner_key = CFString::from_static_string("kCGWindowOwnerName");
                let owner: String = if let Some(value) = dict.find(owner_key.to_void()) {
                    let s: CFString = TCFType::wrap_under_get_rule(*value as *const _);
                    s.to_string()
                } else {
                    continue;
                };
                
                tracing::debug!("Checking window: owner='{}', looking for '{}'", owner, app_name);
                let owner_lower = owner.to_lowercase();
                
                // Check for exact match first (case-insensitive)
                let is_exact_match = owner_lower == app_name_lower;
                
                // Check for fuzzy match (either direction contains)
                let is_fuzzy_match = owner_lower.contains(&app_name_lower) || app_name_lower.contains(&owner_lower);
                
                if is_exact_match || is_fuzzy_match {
                    // Get window ID
                    let window_id_key = CFString::from_static_string("kCGWindowNumber");
                    if let Some(value) = dict.find(window_id_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                        if let Some(id) = num.to_i64() {
                            tracing::debug!("Found candidate: window ID {} for app '{}' (exact={}, fuzzy={})", id, owner, is_exact_match, is_fuzzy_match);
                            
                            // If we found an exact match, use it immediately
                            if is_exact_match {
                                tracing::info!("Found exact match: window ID {} for app '{}'", id, owner);
                                found_window_id = Some((id as u32, owner.clone(), true));
                                break;
                            }
                            
                            // Otherwise, keep the first fuzzy match but continue looking for exact match
                            if found_window_id.is_none() {
                                tracing::info!("Found fuzzy match: window ID {} for app '{}'", id, owner);
                                found_window_id = Some((id as u32, owner.clone(), false));
                            }
                        }
                    }
                }
            }
            
            found_window_id
        };
        
        let (cg_window_id, matched_owner, is_exact) = cg_window_id.ok_or_else(|| {
            anyhow::anyhow!("Could not find window for application '{}'. Use list_windows to see available windows.", app_name)
        })?;
        
        if !is_exact {
            tracing::warn!("Using fuzzy match: requested '{}' but found '{}' (window ID {})", app_name, matched_owner, cg_window_id);
        } else {
            tracing::info!("Taking screenshot of window ID {} for app '{}'", cg_window_id, matched_owner);
        }
        
        // Use screencapture with the window ID for now
        // TODO: Implement direct CGWindowListCreateImage approach with proper image saving
        let mut cmd = std::process::Command::new("screencapture");
        cmd.arg("-x"); // No sound
        cmd.arg("-l");
        cmd.arg(cg_window_id.to_string());
        
        if let Some(region) = region {
            cmd.arg("-R");
            cmd.arg(format!("{},{},{},{}", region.x, region.y, region.width, region.height));
        }
        
        cmd.arg(&final_path);
        
        let screenshot_result = cmd.output()?;
        
        if !screenshot_result.status.success() {
            let stderr = String::from_utf8_lossy(&screenshot_result.stderr);
            return Err(anyhow::anyhow!("screencapture failed for window {}: {}", cg_window_id, stderr));
        }
        
        Ok(())
    }
    
    async fn extract_text_from_screen(&self, region: Rect, window_id: &str) -> Result<String> {
        // Take screenshot of region first
        let temp_path = format!("/tmp/g3_ocr_{}.png", uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, Some(region), Some(window_id)).await?;
        
        // Extract text from the screenshot
        let result = self.extract_text_from_image(&temp_path).await?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        Ok(result)
    }
    
    async fn extract_text_from_image(&self, path: &str) -> Result<String> {
        // Extract all text and concatenate
        let locations = self.ocr_engine.extract_text_with_locations(path).await?;
        Ok(locations.iter().map(|loc| loc.text.as_str()).collect::<Vec<_>>().join(" "))
    }
    
    async fn extract_text_with_locations(&self, path: &str) -> Result<Vec<TextLocation>> {
        // Use the OCR engine
        self.ocr_engine.extract_text_with_locations(path).await
    }
    
    async fn find_text_in_app(&self, app_name: &str, search_text: &str) -> Result<Option<TextLocation>> {
        // Take screenshot of specific app window
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let temp_path = format!("{}/Desktop/g3_find_text_{}_{}.png", home, app_name, uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, None, Some(app_name)).await?;
        
        // Extract all text with locations
        let locations = self.extract_text_with_locations(&temp_path).await?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        // Find matching text (case-insensitive)
        let search_lower = search_text.to_lowercase();
        for location in locations {
            if location.text.to_lowercase().contains(&search_lower) {
                return Ok(Some(location));
            }
        }
        
        Ok(None)
    }
    
    fn move_mouse(&self, x: i32, y: i32) -> Result<()> {
        use core_graphics::event::{
            CGEvent, CGEventTapLocation, CGEventType, CGMouseButton,
        };
        use core_graphics::event_source::{
            CGEventSource, CGEventSourceStateID,
        };
        use core_graphics::geometry::CGPoint;
        
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok().context("Failed to create event source")?;
        
        let event = CGEvent::new_mouse_event(
            source,
            CGEventType::MouseMoved,
            CGPoint::new(x as f64, y as f64),
            CGMouseButton::Left,
        ).ok().context("Failed to create mouse event")?;
        
        event.post(CGEventTapLocation::HID);
        
        Ok(())
    }
    
    fn click_at(&self, x: i32, y: i32, app_name: Option<&str>) -> Result<()> {
        // If app_name is provided, get window position and offset coordinates
        let (global_x, global_y) = if let Some(app) = app_name {
            // Get window position using AppleScript
            let script = format!(
                r#"tell application "{}" to get bounds of window 1"#,
                app
            );
            
            let output = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()?;
            
            if output.status.success() {
                let bounds_str = String::from_utf8_lossy(&output.stdout);
                // Parse bounds: "x1, y1, x2, y2"
                let parts: Vec<&str> = bounds_str.trim().split(", ").collect();
                if parts.len() >= 2 {
                    if let (Ok(window_x), Ok(window_y)) = (
                        parts[0].trim().parse::<i32>(),
                        parts[1].trim().parse::<i32>(),
                    ) {
                        // Offset relative coordinates by window position
                        (x + window_x, y + window_y)
                    } else {
                        (x, y) // Fallback to absolute coordinates
                    }
                } else {
                    (x, y) // Fallback to absolute coordinates
                }
            } else {
                (x, y) // Fallback to absolute coordinates
            }
        } else {
            (x, y) // No app name, use absolute coordinates
        };
        
        use core_graphics::event::{
            CGEvent, CGEventTapLocation, CGEventType, CGMouseButton,
        };
        use core_graphics::event_source::{
            CGEventSource, CGEventSourceStateID,
        };
        use core_graphics::geometry::CGPoint;
        
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok().context("Failed to create event source")?;
        
        let point = CGPoint::new(global_x as f64, global_y as f64);
        
        // Move mouse to position first
        let move_event = CGEvent::new_mouse_event(
            source.clone(),
            CGEventType::MouseMoved,
            point,
            CGMouseButton::Left,
        ).ok().context("Failed to create mouse move event")?;
        move_event.post(CGEventTapLocation::HID);
        
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // Mouse down
        let mouse_down = CGEvent::new_mouse_event(
            source.clone(),
            CGEventType::LeftMouseDown,
            point,
            CGMouseButton::Left,
        ).ok().context("Failed to create mouse down event")?;
        mouse_down.post(CGEventTapLocation::HID);
        
        std::thread::sleep(std::time::Duration::from_millis(50));
        
        // Mouse up
        let mouse_up = CGEvent::new_mouse_event(
            source,
            CGEventType::LeftMouseUp,
            point,
            CGMouseButton::Left,
        ).ok().context("Failed to create mouse up event")?;
        mouse_up.post(CGEventTapLocation::HID);
        
        Ok(())
    }
}