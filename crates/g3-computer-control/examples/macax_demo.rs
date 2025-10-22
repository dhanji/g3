//! Example demonstrating macOS Accessibility API tools
//!
//! This example shows how to use the macax tools to control macOS applications.
//!
//! Run with: cargo run --example macax_demo

use anyhow::Result;
use g3_computer_control::MacAxController;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🍎 macOS Accessibility API Demo\n");
    println!("This demo shows how to control macOS applications using the Accessibility API.\n");
    
    // Create controller
    let controller = MacAxController::new()?;
    println!("✅ MacAxController initialized\n");
    
    // List running applications
    println!("📱 Listing running applications:");
    match controller.list_applications() {
        Ok(apps) => {
            for app in apps.iter().take(10) {
                println!("  - {}", app.name);
            }
            if apps.len() > 10 {
                println!("  ... and {} more", apps.len() - 10);
            }
        }
        Err(e) => println!("  ❌ Error: {}", e),
    }
    println!();
    
    // Get frontmost app
    println!("🎯 Getting frontmost application:");
    match controller.get_frontmost_app() {
        Ok(app) => println!("  Current: {}", app.name),
        Err(e) => println!("  ❌ Error: {}", e),
    }
    println!();
    
    // Example: Activate Finder and get its UI tree
    println!("📂 Activating Finder and inspecting UI:");
    match controller.activate_app("Finder") {
        Ok(_) => {
            println!("  ✅ Finder activated");
            
            // Wait a moment for activation
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            
            // Get UI tree
            match controller.get_ui_tree("Finder", 2) {
                Ok(tree) => {
                    println!("\n  UI Tree:");
                    for line in tree.lines().take(10) {
                        println!("    {}", line);
                    }
                }
                Err(e) => println!("  ❌ Error getting UI tree: {}", e),
            }
        }
        Err(e) => println!("  ❌ Error: {}", e),
    }
    println!();
    
    println!("✨ Demo complete!\n");
    println!("💡 Tips:");
    println!("  - Use --macax flag with g3 to enable these tools");
    println!("  - Grant accessibility permissions in System Preferences");
    println!("  - Add accessibility identifiers to your apps for easier automation");
    println!("  - See docs/macax-tools.md for full documentation\n");
    
    Ok(())
}
