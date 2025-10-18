use g3_computer_control::create_controller;

#[tokio::main]
async fn main() {
    println!("Testing window-specific screenshot capture...");
    
    let controller = create_controller().expect("Failed to create controller");
    
    // Test 1: Capture iTerm2 window
    println!("\n1. Capturing iTerm2 window...");
    match controller.take_screenshot("/tmp/iterm_window.png", None, Some("iTerm2")).await {
        Ok(_) => {
            println!("   ✅ iTerm2 window captured to /tmp/iterm_window.png");
            let _ = std::process::Command::new("open").arg("/tmp/iterm_window.png").spawn();
        }
        Err(e) => println!("   ❌ Failed: {}", e),
    }
    
    // Wait a moment for the image to open
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    
    // Test 2: Full screen capture for comparison
    println!("\n2. Capturing full screen for comparison...");
    match controller.take_screenshot("/tmp/fullscreen.png", None, None).await {
        Ok(_) => {
            println!("   ✅ Full screen captured to /tmp/fullscreen.png");
            let _ = std::process::Command::new("open").arg("/tmp/fullscreen.png").spawn();
        }
        Err(e) => println!("   ❌ Failed: {}", e),
    }
    
    println!("\n=== Comparison ===");
    println!("iTerm window:  /tmp/iterm_window.png (should show ONLY iTerm window)");
    println!("Full screen:   /tmp/fullscreen.png (should show entire desktop)");
    
    // Show file sizes
    if let Ok(meta1) = std::fs::metadata("/tmp/iterm_window.png") {
        if let Ok(meta2) = std::fs::metadata("/tmp/fullscreen.png") {
            println!("\nFile sizes:");
            println!("  iTerm window: {:.1} MB", meta1.len() as f64 / 1_000_000.0);
            println!("  Full screen:  {:.1} MB", meta2.len() as f64 / 1_000_000.0);
            println!("\nWindow capture should be smaller than full screen.");
        }
    }
}
