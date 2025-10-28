use super::{AXApplication, AXElement};
use anyhow::{Context, Result};
use std::collections::HashMap;

#[cfg(target_os = "macos")]
use accessibility::{AXUIElement, AXUIElementAttributes, ElementFinder, TreeVisitor, TreeWalker, TreeWalkerFlow};

#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;

#[cfg(target_os = "macos")]
use core_foundation::string::CFString;

/// macOS Accessibility API controller using native APIs
pub struct MacAxController {
    // Cache for application elements
    app_cache: std::sync::Mutex<HashMap<String, AXUIElement>>,
}

impl MacAxController {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // Check if we have accessibility permissions by trying to get system-wide element
            let _system = AXUIElement::system_wide();
            
            Ok(Self {
                app_cache: std::sync::Mutex::new(HashMap::new()),
            })
        }
        
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("macOS Accessibility API is only available on macOS")
        }
    }
    
    /// List all running applications
    #[cfg(target_os = "macos")]
    pub fn list_applications(&self) -> Result<Vec<AXApplication>> {
        let apps = Self::get_running_applications()?;
        Ok(apps)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn list_applications(&self) -> Result<Vec<AXApplication>> {
        anyhow::bail!("Not supported on this platform")
    }
    
    #[cfg(target_os = "macos")]
    fn get_running_applications() -> Result<Vec<AXApplication>> {
        use cocoa::appkit::NSApplicationActivationPolicy;
        use cocoa::base::{id, nil};
        use objc::{class, msg_send, sel, sel_impl};
        
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let running_apps: id = msg_send![workspace, runningApplications];
            let count: usize = msg_send![running_apps, count];
            
            let mut apps = Vec::new();
            
            for i in 0..count {
                let app: id = msg_send![running_apps, objectAtIndex: i];
                
                // Get app name
                let localized_name: id = msg_send![app, localizedName];
                if localized_name == nil {
                    continue;
                }
                let name_ptr: *const i8 = msg_send![localized_name, UTF8String];
                let name = if !name_ptr.is_null() {
                    std::ffi::CStr::from_ptr(name_ptr)
                        .to_string_lossy()
                        .to_string()
                } else {
                    continue;
                };
                
                // Get bundle ID
                let bundle_id_obj: id = msg_send![app, bundleIdentifier];
                let bundle_id = if bundle_id_obj != nil {
                    let bundle_id_ptr: *const i8 = msg_send![bundle_id_obj, UTF8String];
                    if !bundle_id_ptr.is_null() {
                        Some(
                            std::ffi::CStr::from_ptr(bundle_id_ptr)
                                .to_string_lossy()
                                .to_string(),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                
                // Get PID
                let pid: i32 = msg_send![app, processIdentifier];
                
                // Skip background-only apps
                let activation_policy: i64 = msg_send![app, activationPolicy];
                if activation_policy == NSApplicationActivationPolicy::NSApplicationActivationPolicyRegular as i64 {
                    apps.push(AXApplication {
                        name,
                        bundle_id,
                        pid,
                    });
                }
            }
            
            Ok(apps)
        }
    }
    
    /// Get the frontmost (active) application
    #[cfg(target_os = "macos")]
    pub fn get_frontmost_app(&self) -> Result<AXApplication> {
        use cocoa::base::{id, nil};
        use objc::{class, msg_send, sel, sel_impl};
        
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let frontmost_app: id = msg_send![workspace, frontmostApplication];
            
            if frontmost_app == nil {
                anyhow::bail!("No frontmost application");
            }
            
            // Get app name
            let localized_name: id = msg_send![frontmost_app, localizedName];
            let name_ptr: *const i8 = msg_send![localized_name, UTF8String];
            let name = std::ffi::CStr::from_ptr(name_ptr)
                .to_string_lossy()
                .to_string();
            
            // Get bundle ID
            let bundle_id_obj: id = msg_send![frontmost_app, bundleIdentifier];
            let bundle_id = if bundle_id_obj != nil {
                let bundle_id_ptr: *const i8 = msg_send![bundle_id_obj, UTF8String];
                if !bundle_id_ptr.is_null() {
                    Some(
                        std::ffi::CStr::from_ptr(bundle_id_ptr)
                            .to_string_lossy()
                            .to_string(),
                    )
                } else {
                    None
                }
            } else {
                None
            };
            
            // Get PID
            let pid: i32 = msg_send![frontmost_app, processIdentifier];
            
            Ok(AXApplication {
                name,
                bundle_id,
                pid,
            })
        }
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn get_frontmost_app(&self) -> Result<AXApplication> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Get AXUIElement for an application by name or PID
    #[cfg(target_os = "macos")]
    fn get_app_element(&self, app_name: &str) -> Result<AXUIElement> {
        // Check cache first
        {
            let cache = self.app_cache.lock().unwrap();
            if let Some(element) = cache.get(app_name) {
                return Ok(element.clone());
            }
        }
        
        // Find the app by name
        let apps = Self::get_running_applications()?;
        let app = apps
            .iter()
            .find(|a| a.name == app_name)
            .ok_or_else(|| anyhow::anyhow!("Application '{}' not found", app_name))?;
        
        // Create AXUIElement for the app
        let element = AXUIElement::application(app.pid);
        
        // Cache it
        {
            let mut cache = self.app_cache.lock().unwrap();
            cache.insert(app_name.to_string(), element.clone());
        }
        
        Ok(element)
    }
    
    /// Activate (bring to front) an application
    #[cfg(target_os = "macos")]
    pub fn activate_app(&self, app_name: &str) -> Result<()> {
        use cocoa::base::id;
        use objc::{class, msg_send, sel, sel_impl};
        
        // Find the app
        let apps = Self::get_running_applications()?;
        let app = apps
            .iter()
            .find(|a| a.name == app_name)
            .ok_or_else(|| anyhow::anyhow!("Application '{}' not found", app_name))?;
        
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let running_apps: id = msg_send![workspace, runningApplications];
            let count: usize = msg_send![running_apps, count];
            
            for i in 0..count {
                let running_app: id = msg_send![running_apps, objectAtIndex: i];
                let pid: i32 = msg_send![running_app, processIdentifier];
                
                if pid == app.pid {
                    let _: bool = msg_send![running_app, activateWithOptions: 0];
                    return Ok(());
                }
            }
        }
        
        anyhow::bail!("Failed to activate application")
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn activate_app(&self, _app_name: &str) -> Result<()> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Get the UI hierarchy of an application
    #[cfg(target_os = "macos")]
    pub fn get_ui_tree(&self, app_name: &str, max_depth: usize) -> Result<String> {
        let app_element = self.get_app_element(app_name)?;
        let mut output = format!("Application: {}\n", app_name);
        
        Self::build_ui_tree(&app_element, &mut output, 0, max_depth)?;
        
        Ok(output)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn get_ui_tree(&self, _app_name: &str, _max_depth: usize) -> Result<String> {
        anyhow::bail!("Not supported on this platform")
    }
    
    #[cfg(target_os = "macos")]
    fn build_ui_tree(
        element: &AXUIElement,
        output: &mut String,
        depth: usize,
        max_depth: usize,
    ) -> Result<()> {
        if depth >= max_depth {
            return Ok(());
        }
        
        let indent = "  ".repeat(depth);
        
        // Get role
        let role = element.role().ok().map(|s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        
        // Get title
        let title = element.title().ok()
            .map(|s| s.to_string());
        
        // Get identifier
        let identifier = element.identifier().ok()
            .map(|s| s.to_string());
        
        // Format output
        output.push_str(&format!("{}Role: {}", indent, role));
        if let Some(t) = title {
            output.push_str(&format!(", Title: {}", t));
        }
        if let Some(id) = identifier {
            output.push_str(&format!(", ID: {}", id));
        }
        output.push('\n');
        
        // Get children
        if let Ok(children) = element.children() {
            for i in 0..children.len() {
                if let Some(child) = children.get(i) {
                    let _ = Self::build_ui_tree(&child, output, depth + 1, max_depth);
                }
            }
        }
        
        Ok(())
    }
    
    /// Find UI elements in an application
    #[cfg(target_os = "macos")]
    pub fn find_elements(
        &self,
        app_name: &str,
        role: Option<&str>,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<Vec<AXElement>> {
        let app_element = self.get_app_element(app_name)?;
        let mut found_elements = Vec::new();
        
        let visitor = ElementCollector {
            role_filter: role.map(|s| s.to_string()),
            title_filter: title.map(|s| s.to_string()),
            identifier_filter: identifier.map(|s| s.to_string()),
            results: std::cell::RefCell::new(&mut found_elements),
            depth: std::cell::Cell::new(0),
        };
        
        let walker = TreeWalker::new();
        walker.walk(&app_element, &visitor);
        
        Ok(found_elements)
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn find_elements(
        &self,
        _app_name: &str,
        _role: Option<&str>,
        _title: Option<&str>,
        _identifier: Option<&str>,
    ) -> Result<Vec<AXElement>> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Find a single element (helper for click, set_value, etc.)
    #[cfg(target_os = "macos")]
    fn find_element(
        &self,
        app_name: &str,
        role: &str,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<AXUIElement> {
        let app_element = self.get_app_element(app_name)?;
        
        let role_str = role.to_string();
        let title_str = title.map(|s| s.to_string());
        let identifier_str = identifier.map(|s| s.to_string());
        
        let finder = ElementFinder::new(
            &app_element,
            move |element| {
                // Check role
                let elem_role = element.role()
                    .ok()
                    .map(|s| s.to_string());
                
                if let Some(r) = elem_role {
                    if !r.contains(&role_str) {
                        return false;
                    }
                } else {
                    return false;
                }
                
                // Check title if specified
                if let Some(ref title_filter) = title_str {
                    let elem_title = element.title()
                        .ok()
                        .map(|s| s.to_string());
                    
                    if let Some(t) = elem_title {
                        if !t.contains(title_filter) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                
                // Check identifier if specified
                if let Some(ref id_filter) = identifier_str {
                    let elem_id = element.identifier()
                        .ok()
                        .map(|s| s.to_string());
                    
                    if let Some(id) = elem_id {
                        if !id.contains(id_filter) {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                
                true
            },
            Some(std::time::Duration::from_secs(2)),
        );
        
        finder.find().context("Element not found")
    }
    
    /// Click on a UI element
    #[cfg(target_os = "macos")]
    pub fn click_element(
        &self,
        app_name: &str,
        role: &str,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<()> {
        let element = self.find_element(app_name, role, title, identifier)?;
        
        // Perform the press action
        let action_name = CFString::new("AXPress");
        element
            .perform_action(&action_name)
            .map_err(|e| anyhow::anyhow!("Failed to perform press action: {:?}", e))?;
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn click_element(
        &self,
        _app_name: &str,
        _role: &str,
        _title: Option<&str>,
        _identifier: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Set the value of a UI element
    #[cfg(target_os = "macos")]
    pub fn set_value(
        &self,
        app_name: &str,
        role: &str,
        value: &str,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<()> {
        let element = self.find_element(app_name, role, title, identifier)?;
        
        // Set the value - convert CFString to CFType
        let cf_value = CFString::new(value);
        
        element.set_value(cf_value.as_CFType())
            .map_err(|e| anyhow::anyhow!("Failed to set value: {:?}", e))?;
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn set_value(
        &self,
        _app_name: &str,
        _role: &str,
        _value: &str,
        _title: Option<&str>,
        _identifier: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Get the value of a UI element
    #[cfg(target_os = "macos")]
    pub fn get_value(
        &self,
        app_name: &str,
        role: &str,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<String> {
        let element = self.find_element(app_name, role, title, identifier)?;
        
        // Get the value
        let value_type = element.value()
            .map_err(|e| anyhow::anyhow!("Failed to get value: {:?}", e))?;
        
        // Try to downcast to CFString
        if let Some(cf_string) = value_type.downcast::<CFString>() {
            Ok(cf_string.to_string())
        } else {
            // For non-string values, try to get a description
            Ok(format!("<non-string value>"))
        }
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn get_value(
        &self,
        _app_name: &str,
        _role: &str,
        _title: Option<&str>,
        _identifier: Option<&str>,
    ) -> Result<String> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Type text into the currently focused element (uses system text input)
    #[cfg(target_os = "macos")]
    pub fn type_text(&self, app_name: &str, text: &str) -> Result<()> {
        use cocoa::base::{id, nil};
        use cocoa::foundation::NSString;
        use objc::{class, msg_send, sel, sel_impl};
        
        // First, make sure the app is active
        self.activate_app(app_name)?;
        
        // Wait for app to fully activate
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // Send a Tab key to try to focus on a text field
        // This helps ensure something is focused before we paste
        let _ = self.press_key(app_name, "tab", vec![]);
        std::thread::sleep(std::time::Duration::from_millis(800));
        
        // Save old clipboard, set new content, paste, then restore
        let old_content: id;
        unsafe {
            // Get the general pasteboard
            let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
            
            // Save current clipboard content
            let ns_string_type = NSString::alloc(nil).init_str("public.utf8-plain-text");
            old_content = msg_send![pasteboard, stringForType: ns_string_type];
            
            // Clear and set new content
            let _: () = msg_send![pasteboard, clearContents];
            
            let ns_string = NSString::alloc(nil).init_str(text);
            let ns_type = NSString::alloc(nil).init_str("public.utf8-plain-text");
            let _: bool = msg_send![pasteboard, setString:ns_string forType:ns_type];
        }
        
        // Wait a moment for clipboard to update
        std::thread::sleep(std::time::Duration::from_millis(200));
        
        // Paste using Cmd+V (outside unsafe block)
        self.press_key(app_name, "v", vec!["command"])?;
        
        // Wait for paste to complete
        std::thread::sleep(std::time::Duration::from_millis(300));
        
        // Restore old clipboard content if it existed
        unsafe {
            if old_content != nil {
                let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
                let _: () = msg_send![pasteboard, clearContents];
                let ns_type = NSString::alloc(nil).init_str("public.utf8-plain-text");
                let _: bool = msg_send![pasteboard, setString:old_content forType:ns_type];
            }
        }
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn type_text(&self, _app_name: &str, _text: &str) -> Result<()> {
        anyhow::bail!("Not supported on this platform")
    }
    
    /// Focus on a text field or text area element
    #[cfg(target_os = "macos")]
    pub fn focus_element(
        &self,
        app_name: &str,
        role: &str,
        title: Option<&str>,
        identifier: Option<&str>,
    ) -> Result<()> {
        let element = self.find_element(app_name, role, title, identifier)?;
        
        // Set focused attribute to true
        use core_foundation::boolean::CFBoolean;
        let cf_true = CFBoolean::true_value();
        
        element.set_attribute(&accessibility::AXAttribute::focused(), cf_true)
            .map_err(|e| anyhow::anyhow!("Failed to focus element: {:?}", e))?;
        
        Ok(())
    }
    
    /// Press a keyboard shortcut
    #[cfg(target_os = "macos")]
    pub fn press_key(
        &self,
        app_name: &str,
        key: &str,
        modifiers: Vec<&str>,
    ) -> Result<()> {
        use core_graphics::event::{
            CGEvent, CGEventFlags, CGEventTapLocation,
        };
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
        
        // First, make sure the app is active
        self.activate_app(app_name)?;
        
        // Wait a bit for activation
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // Map key string to key code
        let key_code = Self::key_to_keycode(key)
            .ok_or_else(|| anyhow::anyhow!("Unknown key: {}", key))?;
        
        // Map modifiers to flags
        let mut flags = CGEventFlags::CGEventFlagNull;
        for modifier in modifiers {
            match modifier.to_lowercase().as_str() {
                "command" | "cmd" => flags |= CGEventFlags::CGEventFlagCommand,
                "option" | "alt" => flags |= CGEventFlags::CGEventFlagAlternate,
                "control" | "ctrl" => flags |= CGEventFlags::CGEventFlagControl,
                "shift" => flags |= CGEventFlags::CGEventFlagShift,
                _ => {}
            }
        }
        
        // Create event source
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok().context("Failed to create event source")?;
        
        // Create key down event
        let key_down = CGEvent::new_keyboard_event(source.clone(), key_code, true)
            .ok().context("Failed to create key down event")?;
        key_down.set_flags(flags);
        
        // Create key up event
        let key_up = CGEvent::new_keyboard_event(source, key_code, false)
            .ok().context("Failed to create key up event")?;
        key_up.set_flags(flags);
        
        // Post events
        key_down.post(CGEventTapLocation::HID);
        std::thread::sleep(std::time::Duration::from_millis(50));
        key_up.post(CGEventTapLocation::HID);
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    pub fn press_key(
        &self,
        _app_name: &str,
        _key: &str,
        _modifiers: Vec<&str>,
    ) -> Result<()> {
        anyhow::bail!("Not supported on this platform")
    }
    
    #[cfg(target_os = "macos")]
    fn key_to_keycode(key: &str) -> Option<u16> {
        // Map common keys to keycodes
        // See: https://eastmanreference.com/complete-list-of-applescript-key-codes
        match key.to_lowercase().as_str() {
            "a" => Some(0x00),
            "s" => Some(0x01),
            "d" => Some(0x02),
            "f" => Some(0x03),
            "h" => Some(0x04),
            "g" => Some(0x05),
            "z" => Some(0x06),
            "x" => Some(0x07),
            "c" => Some(0x08),
            "v" => Some(0x09),
            "b" => Some(0x0B),
            "q" => Some(0x0C),
            "w" => Some(0x0D),
            "e" => Some(0x0E),
            "r" => Some(0x0F),
            "y" => Some(0x10),
            "t" => Some(0x11),
            "1" => Some(0x12),
            "2" => Some(0x13),
            "3" => Some(0x14),
            "4" => Some(0x15),
            "6" => Some(0x16),
            "5" => Some(0x17),
            "=" => Some(0x18),
            "9" => Some(0x19),
            "7" => Some(0x1A),
            "-" => Some(0x1B),
            "8" => Some(0x1C),
            "0" => Some(0x1D),
            "]" => Some(0x1E),
            "o" => Some(0x1F),
            "u" => Some(0x20),
            "[" => Some(0x21),
            "i" => Some(0x22),
            "p" => Some(0x23),
            "return" | "enter" => Some(0x24),
            "l" => Some(0x25),
            "j" => Some(0x26),
            "'" => Some(0x27),
            "k" => Some(0x28),
            ";" => Some(0x29),
            "\\" => Some(0x2A),
            "," => Some(0x2B),
            "/" => Some(0x2C),
            "n" => Some(0x2D),
            "m" => Some(0x2E),
            "." => Some(0x2F),
            "tab" => Some(0x30),
            "space" => Some(0x31),
            "`" => Some(0x32),
            "delete" | "backspace" => Some(0x33),
            "escape" | "esc" => Some(0x35),
            "f1" => Some(0x7A),
            "f2" => Some(0x78),
            "f3" => Some(0x63),
            "f4" => Some(0x76),
            "f5" => Some(0x60),
            "f6" => Some(0x61),
            "f7" => Some(0x62),
            "f8" => Some(0x64),
            "f9" => Some(0x65),
            "f10" => Some(0x6D),
            "f11" => Some(0x67),
            "f12" => Some(0x6F),
            "left" => Some(0x7B),
            "right" => Some(0x7C),
            "down" => Some(0x7D),
            "up" => Some(0x7E),
            _ => None,
        }
    }
}

#[cfg(target_os = "macos")]
struct ElementCollector<'a> {
    role_filter: Option<String>,
    title_filter: Option<String>,
    identifier_filter: Option<String>,
    results: std::cell::RefCell<&'a mut Vec<AXElement>>,
    depth: std::cell::Cell<usize>,
}

#[cfg(target_os = "macos")]
impl<'a> TreeVisitor for ElementCollector<'a> {
    fn enter_element(&self, element: &AXUIElement) -> TreeWalkerFlow {
        self.depth.set(self.depth.get() + 1);
        
        if self.depth.get() > 20 {
            return TreeWalkerFlow::SkipSubtree;
        }
        
        // Get element properties
        let role = element.role()
            .ok()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        
        let title = element.title()
            .ok()
            .map(|s| s.to_string());
        
        let identifier = element.identifier()
            .ok()
            .map(|s| s.to_string());
        
        // Check if this element matches the filters
        let role_matches = self.role_filter.as_ref().map_or(true, |r| role.contains(r));
        let title_matches = self.title_filter.as_ref().map_or(true, |t| {
            title.as_ref().map_or(false, |title_str| title_str.contains(t))
        });
        let identifier_matches = self.identifier_filter.as_ref().map_or(true, |id| {
            identifier.as_ref().map_or(false, |id_str| id_str.contains(id))
        });
        
        if role_matches && title_matches && identifier_matches {
            // Get additional properties
            let value = element.value()
                .ok()
                .and_then(|v| {
                    v.downcast::<CFString>().map(|s| s.to_string())
                });
            
            let label = element.description()
                .ok()
                .map(|s| s.to_string());
            
            let enabled = element.enabled()
                .ok()
                .map(|b| b.into())
                .unwrap_or(false);
            
            let focused = element.focused()
                .ok()
                .map(|b| b.into())
                .unwrap_or(false);
            
            // Count children
            let children_count = element.children()
                .ok()
                .map(|arr| arr.len() as usize)
                .unwrap_or(0);
            
            self.results.borrow_mut().push(AXElement {
                role,
                title,
                value,
                label,
                identifier,
                enabled,
                focused,
                position: None,
                size: None,
                children_count,
            });
        }
        
        TreeWalkerFlow::Continue
    }
    
    fn exit_element(&self, _element: &AXUIElement) {
        self.depth.set(self.depth.get() - 1);
    }
}
