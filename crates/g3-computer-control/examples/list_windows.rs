use core_graphics::window::{kCGWindowListOptionOnScreenOnly, kCGNullWindowID, CGWindowListCopyWindowInfo};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation::base::TCFType;

fn main() {
    println!("Listing all on-screen windows...");
    println!("{:<10} {:<25} {}", "Window ID", "Owner", "Title");
    println!("{}", "-".repeat(80));
    
    unsafe {
        let window_list = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly,
            kCGNullWindowID
        );
        
        let count = core_foundation::array::CFArray::<CFDictionary>::wrap_under_create_rule(window_list).len();
        let array = core_foundation::array::CFArray::<CFDictionary>::wrap_under_create_rule(window_list);
        
        for i in 0..count {
            let dict = array.get(i).unwrap();
            
            // Get window ID
            let window_id_key = CFString::from_static_string("kCGWindowNumber");
            let window_id: i64 = if let Some(value) = dict.find(window_id_key.as_concrete_TypeRef()) {
                let num: core_foundation::number::CFNumber = TCFType::wrap_under_get_rule(*value as *const _);
                num.to_i64().unwrap_or(0)
            } else {
                0
            };
            
            // Get owner name
            let owner_key = CFString::from_static_string("kCGWindowOwnerName");
            let owner: String = if let Some(value) = dict.find(owner_key.as_concrete_TypeRef()) {
                let s: CFString = TCFType::wrap_under_get_rule(*value as *const _);
                s.to_string()
            } else {
                "Unknown".to_string()
            };
            
            // Get window name/title
            let name_key = CFString::from_static_string("kCGWindowName");
            let title: String = if let Some(value) = dict.find(name_key.as_concrete_TypeRef()) {
                let s: CFString = TCFType::wrap_under_get_rule(*value as *const _);
                s.to_string()
            } else {
                "".to_string()
            };
            
            // Filter for iTerm or show all
            if owner.contains("iTerm") || owner.contains("Terminal") {
                println!("{:<10} {:<25} {}", window_id, owner, title);
            }
        }
    }
}
