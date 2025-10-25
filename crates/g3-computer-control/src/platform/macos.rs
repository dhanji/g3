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
            
            let mut found_window_id: Option<(u32, String)> = None; // (id, owner)
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
                
                // Normalize by removing spaces for exact matching
                let app_name_normalized = app_name_lower.replace(" ", "");
                let owner_normalized = owner_lower.replace(" ", "");
                
                // ONLY accept exact matches (case-insensitive, with or without spaces)
                // This prevents "Goose" from matching "GooseStudio"
                let is_match = owner_lower == app_name_lower || owner_normalized == app_name_normalized;
                
                if is_match {
                    // Get window ID
                    let window_id_key = CFString::from_static_string("kCGWindowNumber");
                    if let Some(value) = dict.find(window_id_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                        if let Some(id) = num.to_i64() {
                            // Get window layer to filter out menu bar windows
                            let layer_key = CFString::from_static_string("kCGWindowLayer");
                            let layer: i32 = if let Some(value) = dict.find(layer_key.to_void()) {
                                let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                                num.to_i32().unwrap_or(0)
                            } else {
                                0
                            };
                            
                            // Get window bounds to verify it's a real window
                            let bounds_key = CFString::from_static_string("kCGWindowBounds");
                            let has_real_bounds = if let Some(value) = dict.find(bounds_key.to_void()) {
                                let bounds_dict: CFDictionary = TCFType::wrap_under_get_rule(*value as *const _);
                                let width_key = CFString::from_static_string("Width");
                                let height_key = CFString::from_static_string("Height");
                                
                                if let (Some(w_val), Some(h_val)) = (
                                    bounds_dict.find(width_key.to_void()),
                                    bounds_dict.find(height_key.to_void()),
                                ) {
                                    let w_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*w_val as *const _);
                                    let h_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*h_val as *const _);
                                    let width = w_num.to_f64().unwrap_or(0.0);
                                    let height = h_num.to_f64().unwrap_or(0.0);
                                    // Real windows should be at least 100x100 pixels
                                    width >= 100.0 && height >= 100.0
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            
                            // Only accept windows that are:
                            // 1. At layer 0 (normal windows, not menu bar)
                            // 2. Have real bounds (width and height >= 100)
                            if layer == 0 && has_real_bounds {
                                tracing::info!("Found valid window: ID {} for app '{}' (layer={}, bounds valid)", id, owner, layer);
                                found_window_id = Some((id as u32, owner.clone()));
                                break;
                            } else {
                                tracing::debug!("Skipping window ID {} for '{}': layer={}, has_real_bounds={}", id, owner, layer, has_real_bounds);
                            }
                        }
                    }
                }
            }
            
            found_window_id
        };
        
        let (cg_window_id, matched_owner) = cg_window_id.ok_or_else(|| {
            anyhow::anyhow!("Could not find window for application '{}'. Use list_windows to see available windows.", app_name)
        })?;
            tracing::info!("Taking screenshot of window ID {} for app '{}'", cg_window_id, matched_owner);
        
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
        let temp_path = format!("{}/tmp/g3_find_text_{}_{}.png", home, app_name, uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, None, Some(app_name)).await?;
        
        // Get screenshot dimensions before we delete it
        let screenshot_dims = get_image_dimensions(&temp_path)?;
        
        // Extract all text with locations
        let locations = self.extract_text_with_locations(&temp_path).await?;
        
        // Get window bounds to calculate coordinate transformation
        let window_bounds = self.get_window_bounds(app_name)?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        // Find matching text (case-insensitive)
        let search_lower = search_text.to_lowercase();
        for location in locations {
            if location.text.to_lowercase().contains(&search_lower) {
                // Transform coordinates from screenshot space to screen space
                let transformed = transform_screenshot_to_screen_coords(
                    location,
                    window_bounds,
                    screenshot_dims,
                );
                return Ok(Some(transformed));
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
    
    fn click_at(&self, x: i32, y: i32, _app_name: Option<&str>) -> Result<()> {
        use core_graphics::event::{
            CGEvent, CGEventTapLocation, CGEventType, CGMouseButton,
        };
        use core_graphics::event_source::{
            CGEventSource, CGEventSourceStateID,
        };
        use core_graphics::geometry::CGPoint;
        use core_graphics::display::CGDisplay;
        
        // IMPORTANT: Coordinates passed here are in NSScreen/CGWindowListCopyWindowInfo space
        // (Y=0 at BOTTOM, increases UPWARD)
        // But CGEvent uses a different coordinate system (Y=0 at TOP, increases DOWNWARD)
        // We need to convert: CGEvent.y = screenHeight - NSScreen.y
        
        let screen_height = CGDisplay::main().pixels_high() as i32;
        let cgevent_x = x;
        let cgevent_y = screen_height - y;
        
        tracing::debug!("click_at: NSScreen coords ({}, {}) -> CGEvent coords ({}, {}) [screen_height={}]",
            x, y, cgevent_x, cgevent_y, screen_height);
        
        let (global_x, global_y) = (cgevent_x, cgevent_y);
        
        let point = CGPoint::new(global_x as f64, global_y as f64);
        
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok().context("Failed to create event source")?;
        
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

impl MacOSController {
    /// Get window bounds for an application (helper method)
    fn get_window_bounds(&self, app_name: &str) -> Result<(i32, i32, i32, i32)> {
        unsafe {
            let window_list = CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly,
                kCGNullWindowID
            );
            
            let array = CFArray::<CFDictionary>::wrap_under_create_rule(window_list);
            let count = array.len();
            
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
                
                let owner_lower = owner.to_lowercase();
                
                // Normalize by removing spaces for exact matching
                let app_name_normalized = app_name_lower.replace(" ", "");
                let owner_normalized = owner_lower.replace(" ", "");
                
                // ONLY accept exact matches (case-insensitive, with or without spaces)
                // This prevents "Goose" from matching "GooseStudio"
                let is_match = owner_lower == app_name_lower || owner_normalized == app_name_normalized;
                
                if is_match {
                    // Get window layer to filter out menu bar windows
                    let layer_key = CFString::from_static_string("kCGWindowLayer");
                    let layer: i32 = if let Some(value) = dict.find(layer_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                        num.to_i32().unwrap_or(0)
                    } else {
                        0
                    };
                    
                    // Skip menu bar windows (layer >= 20)
                    if layer >= 20 {
                        tracing::debug!("Skipping window for '{}' at layer {} (menu bar)", owner, layer);
                        continue;
                    }
                    
                    // Get window bounds to verify it's a real window
                    let bounds_key = CFString::from_static_string("kCGWindowBounds");
                    if let Some(value) = dict.find(bounds_key.to_void()) {
                        let bounds_dict: CFDictionary = TCFType::wrap_under_get_rule(*value as *const _);
                        
                        let x_key = CFString::from_static_string("X");
                        let y_key = CFString::from_static_string("Y");
                        let width_key = CFString::from_static_string("Width");
                        let height_key = CFString::from_static_string("Height");
                        
                        if let (Some(x_val), Some(y_val), Some(w_val), Some(h_val)) = (
                            bounds_dict.find(x_key.to_void()),
                            bounds_dict.find(y_key.to_void()),
                            bounds_dict.find(width_key.to_void()),
                            bounds_dict.find(height_key.to_void()),
                        ) {
                            let x_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*x_val as *const _);
                            let y_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*y_val as *const _);
                            let w_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*w_val as *const _);
                            let h_num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*h_val as *const _);
                            
                            let x: i32 = x_num.to_i64().unwrap_or(0) as i32;
                            let y: i32 = y_num.to_i64().unwrap_or(0) as i32;
                            let w: i32 = w_num.to_i64().unwrap_or(0) as i32;
                            let h: i32 = h_num.to_i64().unwrap_or(0) as i32;
                            
                            // Only accept windows with real bounds (>= 100x100 pixels)
                            if w >= 100 && h >= 100 {
                                tracing::info!("Found valid window bounds for '{}': x={}, y={}, w={}, h={} (layer={})", owner, x, y, w, h, layer);
                                return Ok((x, y, w, h));
                            } else {
                                tracing::debug!("Skipping window for '{}': too small ({}x{})", owner, w, h);
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                }
            }
        }
        
        Err(anyhow::anyhow!("Could not find window bounds for '{}'", app_name))
    }
}

/// Get image dimensions from a PNG file
fn get_image_dimensions(path: &str) -> Result<(i32, i32)> {
    use std::fs::File;
    use std::io::Read;
    
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 24];
    file.read_exact(&mut buffer)?;
    
    // PNG signature check
    if &buffer[0..8] != b"\x89PNG\r\n\x1a\n" {
        anyhow::bail!("Not a valid PNG file");
    }
    
    // Read IHDR chunk (width and height are at bytes 16-23)
    let width = u32::from_be_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]) as i32;
    let height = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]) as i32;
    
    Ok((width, height))
}

/// Transform coordinates from screenshot space to screen space
/// 
/// The screenshot is taken of a window, and Vision OCR returns coordinates
/// relative to the screenshot image. We need to transform these to actual
/// screen coordinates for clicking.
/// 
/// On Retina displays, screenshots are taken at 2x resolution, so we need
/// to account for this scaling factor.
fn transform_screenshot_to_screen_coords(
    location: TextLocation,
    window_bounds: (i32, i32, i32, i32), // (x, y, width, height) in screen space
    screenshot_dims: (i32, i32), // (width, height) in pixels
) -> TextLocation {
    let (win_x, win_y, win_width, win_height) = window_bounds;
    let (screenshot_width, screenshot_height) = screenshot_dims;
    
    // Calculate scale factors
    // On Retina displays, screenshot is typically 2x the window size
    let scale_x = win_width as f64 / screenshot_width as f64;
    let scale_y = win_height as f64 / screenshot_height as f64;
    
    tracing::debug!("Transform: screenshot={}x{}, window={}x{} at ({},{}), scale=({:.2},{:.2})",
        screenshot_width, screenshot_height, win_width, win_height, win_x, win_y, scale_x, scale_y);
    
    // Transform coordinates from image space to screen space
    // IMPORTANT: macOS screen coordinates have origin at BOTTOM-LEFT (Y increases upward)
    // Image coordinates have origin at TOP-LEFT (Y increases downward)
    // win_y is the BOTTOM of the window in screen coordinates
    // So we need to: (win_y + win_height) to get window TOP, then subtract screenshot_y
    let window_top_y = win_y + win_height;
    
    tracing::debug!("[transform] Input location in image space: x={}, y={}, width={}, height={}",
        location.x, location.y, location.width, location.height);
    tracing::debug!("[transform] Scale factors: scale_x={:.4}, scale_y={:.4}", scale_x, scale_y);
    
    let transformed_x = win_x + (location.x as f64 * scale_x) as i32;
    let transformed_y = window_top_y - (location.y as f64 * scale_y) as i32;
    let transformed_width = (location.width as f64 * scale_x) as i32;
    let transformed_height = (location.height as f64 * scale_y) as i32;
    
    tracing::debug!("[transform] Calculation details:");
    tracing::debug!("  - transformed_x = {} + ({} * {:.4}) = {} + {:.2} = {}", win_x, location.x, scale_x, win_x, location.x as f64 * scale_x, transformed_x);
    tracing::debug!("  - transformed_width = ({} * {:.4}) = {:.2} -> {}", location.width, scale_x, location.width as f64 * scale_x, transformed_width);
    tracing::debug!("  - transformed_height = ({} * {:.4}) = {:.2} -> {}", location.height, scale_y, location.height as f64 * scale_y, transformed_height);
    
    tracing::debug!("Transformed location: screenshot=({},{}) {}x{} -> screen=({},{}) {}x{}",
        location.x, location.y, location.width, location.height,
        transformed_x, transformed_y, transformed_width, transformed_height);
    
    TextLocation {
        text: location.text,
        x: transformed_x,
        y: transformed_y,
        width: transformed_width,
        height: transformed_height,
        confidence: location.confidence,
    }
}

#[path = "macos_window_matching_test.rs"]
#[cfg(test)]
mod tests;