use crate::{ComputerController, types::*};
use anyhow::Result;
use async_trait::async_trait;
use core_graphics::display::CGPoint;
use core_graphics::event::{CGEvent, CGEventType, CGMouseButton, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::path::Path;
use tesseract::Tesseract;
use core_graphics::window::{kCGWindowListOptionOnScreenOnly, kCGNullWindowID, CGWindowListCopyWindowInfo};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation::base::{TCFType, ToVoid};

// MacOSController doesn't store CGEventSource to avoid Send/Sync issues
// We create it fresh for each operation
pub struct MacOSController {
    // Empty struct - event source created per operation
}

impl MacOSController {
    pub fn new() -> Result<Self> {
        // Test that we can create an event source
        let _event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| anyhow::anyhow!("Failed to create event source. Make sure Accessibility permissions are granted."))?;
        Ok(Self {})
    }
    
    fn key_to_keycode(&self, key: &str) -> Result<u16> {
        // Map key names to macOS keycodes
        let keycode = match key.to_lowercase().as_str() {
            "return" | "enter" => 36,
            "tab" => 48,
            "space" => 49,
            "delete" | "backspace" => 51,
            "escape" | "esc" => 53,
            "command" | "cmd" => 55,
            "shift" => 56,
            "capslock" => 57,
            "option" | "alt" => 58,
            "control" | "ctrl" => 59,
            "left" => 123,
            "right" => 124,
            "down" => 125,
            "up" => 126,
            _ => anyhow::bail!("Unknown key: {}", key),
        };
        Ok(keycode)
    }
}

#[async_trait]
impl ComputerController for MacOSController {
    async fn move_mouse(&self, x: i32, y: i32) -> Result<()> {
        let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
        let point = CGPoint::new(x as f64, y as f64);
        let event = CGEvent::new_mouse_event(
            event_source,
            CGEventType::MouseMoved,
            point,
            CGMouseButton::Left,
        ).map_err(|_| anyhow::anyhow!("Failed to create mouse move event"))?;
        
        event.post(CGEventTapLocation::HID);
        Ok(())
    }
    
    async fn click(&self, button: MouseButton) -> Result<()> {
        let (cg_button, down_type, up_type) = match button {
            MouseButton::Left => (CGMouseButton::Left, CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
            MouseButton::Right => (CGMouseButton::Right, CGEventType::RightMouseDown, CGEventType::RightMouseUp),
            MouseButton::Middle => (CGMouseButton::Center, CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
        };
        
        let point = {
            // Get current mouse position
            let temp_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
            let event = CGEvent::new(temp_source)
                .map_err(|_| anyhow::anyhow!("Failed to get mouse position"))?;
            let p = event.location();
            p
        };
        
        {
            let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
            
            // Mouse down
            let down_event = CGEvent::new_mouse_event(
                event_source,
                down_type,
                point,
                cg_button,
            ).map_err(|_| anyhow::anyhow!("Failed to create mouse down event"))?;
            down_event.post(CGEventTapLocation::HID);
        } // event_source and down_event dropped here
        
        // Small delay
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        
        {
            let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
            
            let up_event = CGEvent::new_mouse_event(
                event_source,
                up_type,
                point,
                cg_button,
            ).map_err(|_| anyhow::anyhow!("Failed to create mouse up event"))?;
            up_event.post(CGEventTapLocation::HID);
        } // event_source and up_event dropped here
        
        Ok(())
    }
    
    async fn double_click(&self, button: MouseButton) -> Result<()> {
        self.click(button).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        self.click(button).await?;
        Ok(())
    }
    
    async fn type_text(&self, text: &str) -> Result<()> {
        for ch in text.chars() {
            {
                let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                    .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
                
                // Create keyboard event for character
                let event = CGEvent::new_keyboard_event(
                    event_source,
                    0, // keycode (0 for unicode)
                    true,
                ).map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;
                
                // Set unicode string
                let mut utf16_buf = [0u16; 2];
                let utf16_slice = ch.encode_utf16(&mut utf16_buf);
                let utf16_chars: Vec<u16> = utf16_slice.iter().copied().collect();
                
                event.set_string_from_utf16_unchecked(utf16_chars.as_slice());
                event.post(CGEventTapLocation::HID);
            } // event_source and event dropped here
            
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }
    
    async fn press_key(&self, key: &str) -> Result<()> {
        let keycode = self.key_to_keycode(key)?;
        
        {
            let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
            
            // Key down
            let down_event = CGEvent::new_keyboard_event(
                event_source,
                keycode,
                true,
            ).map_err(|_| anyhow::anyhow!("Failed to create key down event"))?;
            down_event.post(CGEventTapLocation::HID);
        } // event_source and down_event dropped here
        
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        
        {
            let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|_| anyhow::anyhow!("Failed to create event source"))?;
            
            // Key up
            let up_event = CGEvent::new_keyboard_event(
                event_source,
                keycode,
                false,
            ).map_err(|_| anyhow::anyhow!("Failed to create key up event"))?;
            up_event.post(CGEventTapLocation::HID);
        } // event_source and up_event dropped here
        
        Ok(())
    }
    
    async fn list_windows(&self) -> Result<Vec<Window>> {
        let mut windows = Vec::new();
        
        unsafe {
            let window_list = CGWindowListCopyWindowInfo(
                kCGWindowListOptionOnScreenOnly,
                kCGNullWindowID
            );
            
            let array = core_foundation::array::CFArray::<CFDictionary>::wrap_under_create_rule(window_list);
            let count = array.len();
            
            for i in 0..count {
                let dict = array.get(i).unwrap();
                
                // Get window ID
                let window_id_key = CFString::from_static_string("kCGWindowNumber");
                let window_id: i64 = if let Some(value) = dict.find(window_id_key.to_void()) {
                    let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                    num.to_i64().unwrap_or(0)
                } else {
                    0
                };
                
                // Get owner name (app name)
                let owner_key = CFString::from_static_string("kCGWindowOwnerName");
                let app_name: String = if let Some(value) = dict.find(owner_key.to_void()) {
                    let s: CFString = TCFType::wrap_under_get_rule(*value as *const _);
                    s.to_string()
                } else {
                    "Unknown".to_string()
                };
                
                // Get window name/title
                let name_key = CFString::from_static_string("kCGWindowName");
                let title: String = if let Some(value) = dict.find(name_key.to_void()) {
                    let s: CFString = TCFType::wrap_under_get_rule(*value as *const _);
                    s.to_string()
                } else {
                    "".to_string()
                };
                
                // Get window bounds
                let bounds_key = CFString::from_static_string("kCGWindowBounds");
                let bounds = if let Some(bounds_value) = dict.find(bounds_key.to_void()) {
                    let bounds_dict: CFDictionary = TCFType::wrap_under_get_rule(*bounds_value as *const _);
                    
                    let x_key = CFString::from_static_string("X");
                    let y_key = CFString::from_static_string("Y");
                    let width_key = CFString::from_static_string("Width");
                    let height_key = CFString::from_static_string("Height");
                    
                    let x = if let Some(x_value) = bounds_dict.find(x_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*x_value as *const _);
                        num.to_i32().unwrap_or(0)
                    } else { 0 };
                    let y = if let Some(y_value) = bounds_dict.find(y_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*y_value as *const _);
                        num.to_i32().unwrap_or(0)
                    } else { 0 };
                    let width = if let Some(width_value) = bounds_dict.find(width_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*width_value as *const _);
                        num.to_i32().unwrap_or(0)
                    } else { 0 };
                    let height = if let Some(height_value) = bounds_dict.find(height_key.to_void()) {
                        let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*height_value as *const _);
                        num.to_i32().unwrap_or(0)
                    } else { 0 };
                    
                    Rect { x, y, width, height }
                } else {
                    Rect { x: 0, y: 0, width: 0, height: 0 }
                };
                
                // Skip windows without meaningful content (system UI elements, etc.)
                if app_name.is_empty() || (title.is_empty() && bounds.width < 100) {
                    continue;
                }
                
                windows.push(Window {
                    id: format!("{}:{}", app_name, window_id),
                    title,
                    app_name,
                    bounds,
                    is_active: false, // We'd need additional API calls to determine this
                });
            }
        }
        
        Ok(windows)
    }
    
    async fn focus_window(&self, _window_id: &str) -> Result<()> {
        // Note: Full implementation would use NSWorkspace to activate application
        tracing::warn!("focus_window not fully implemented on macOS");
        Ok(())
    }
    
    async fn get_window_bounds(&self, _window_id: &str) -> Result<Rect> {
        // Note: Full implementation would use Accessibility API
        tracing::warn!("get_window_bounds not fully implemented on macOS");
        Ok(Rect { x: 0, y: 0, width: 800, height: 600 })
    }
    
    async fn find_element(&self, _selector: &ElementSelector) -> Result<Option<UIElement>> {
        // Note: Full implementation would use macOS Accessibility API
        tracing::warn!("find_element not fully implemented on macOS");
        Ok(None)
    }
    
    async fn get_element_text(&self, _element_id: &str) -> Result<String> {
        // Note: Full implementation would use Accessibility API
        tracing::warn!("get_element_text not fully implemented on macOS");
        Ok(String::new())
    }
    
    async fn get_element_bounds(&self, _element_id: &str) -> Result<Rect> {
        // Note: Full implementation would use Accessibility API
        tracing::warn!("get_element_bounds not fully implemented on macOS");
        Ok(Rect { x: 0, y: 0, width: 100, height: 30 })
    }
    
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
        
        // Get the currently focused application before taking screenshot
        let current_app = std::process::Command::new("osascript")
            .arg("-e")
            .arg("tell application \"System Events\" to get name of first application process whose frontmost is true")
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                } else {
                    None
                }
            });
        
        // Handle application-based window capture
        let app_name_opt = window_id.and_then(|id| {
            // Extract app name from window_id format "AppName:WindowNumber"
            id.split(':').next().map(String::from)
        });
        
        // If we're capturing a specific window, foreground it first
        if let Some(ref app) = app_name_opt {
            tracing::debug!("Foregrounding application: {}", app);
            let _ = std::process::Command::new("osascript")
                .arg("-e")
                .arg(format!("tell application \"{}\" to activate", app))
                .output();
            
            // Give the window time to come to the front
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        
        let screenshot_result = if let Some(ref app) = app_name_opt {
            // Use screencapture with AppleScript to get window ID
            let script = format!(
                r#"tell application "{}" to id of window 1"#,
                app
            );
            
            let output = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output()?;
            
            if output.status.success() {
                let window_id_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                std::process::Command::new("screencapture")
                    .arg(format!("-l{}", window_id_str))
                    .arg("-o")
                    .arg(&final_path)
                    .output()
            } else {
                // Fallback to regular screenshot if we can't get window ID
                std::process::Command::new("screencapture")
                    .arg("-x")
                    .arg(&final_path)
                    .output()
            }
        } else {
            // Regular screenshot (full screen or region)
        // Use native macOS screencapture command which handles all the format complexities
        
        // Check if we have Screen Recording permission by attempting a test capture
        // If we only get wallpaper/menubar but no windows, we need permission
        let needs_permission_check = std::env::var("G3_SKIP_PERMISSION_CHECK").is_err();
        
        if needs_permission_check {
            // Try to open Screen Recording settings if this is the first screenshot
            static PERMISSION_PROMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            
            if !PERMISSION_PROMPTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("\n=== Screen Recording Permission Required ===\n\
                    macOS requires explicit permission to capture window content.\n\
                    If screenshots only show wallpaper/menubar (no windows):\n\n\
                    1. Open System Settings > Privacy & Security > Screen Recording\n\
                    2. Enable permission for your terminal (iTerm/Terminal) or g3\n\
                    3. Restart your terminal if needed\n\n\
                    Opening Screen Recording settings now...\n");
                
                // Try to open the settings (non-blocking)
                let _ = std::process::Command::new("open")
                    .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                    .spawn();
            }
        }
        
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
        
        cmd.arg(&final_path);
        
            cmd.output()
        }?;
        
        if !screenshot_result.status.success() {
            let stderr = String::from_utf8_lossy(&screenshot_result.stderr);
            return Err(anyhow::anyhow!("screencapture failed: {}", stderr));
        }
        
        // Re-foreground the original application if we foregrounded a different window
        if let Some(ref target_app) = app_name_opt {
            if let Some(ref original_app) = current_app {
                // Only restore if we actually changed the foreground app
                if target_app != original_app {
                    tracing::debug!("Restoring focus to original application: {}", original_app);
                    
                    // Small delay to ensure screenshot is complete
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    
                    let _ = std::process::Command::new("osascript")
                        .arg("-e")
                    .arg(format!("tell application \"{}\" to activate", original_app))
                    .output();
                }
            }
        }
        
        tracing::debug!("Screenshot saved using screencapture: {}", final_path);
        
        Ok(())
    }
    
    
    async fn extract_text_from_screen(&self, region: Rect) -> Result<OCRResult> {
        // Take screenshot of region first
        let temp_path = format!("/tmp/g3_ocr_{}.png", uuid::Uuid::new_v4());
        self.take_screenshot(&temp_path, Some(region), None).await?;
        
        // Extract text from the screenshot
        let result = self.extract_text_from_image(&temp_path).await?;
        
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
        
        Ok(result)
    }
    
    async fn extract_text_from_image(&self, _path: &str) -> Result<OCRResult> {
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
                To install tesseract:\n  macOS:   brew install tesseract\n  \
                Linux:   sudo apt-get install tesseract-ocr (Ubuntu/Debian)\n           \
                sudo yum install tesseract (RHEL/CentOS)\n  \
                Windows: Download from https://github.com/UB-Mannheim/tesseract/wiki\n\n\
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
                    macOS:   brew reinstall tesseract\n  \
                    Linux:   sudo apt-get install tesseract-ocr-eng\n  \
                    Windows: Reinstall tesseract and ensure language files are included", e)
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
