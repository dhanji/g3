//! Test the new type_text functionality

use anyhow::Result;
use g3_computer_control::MacAxController;

#[tokio::main]
async fn main() -> Result<()> {
    println!("🧪 Testing macax type_text functionality\n");
    
    let controller = MacAxController::new()?;
    println!("✅ Controller initialized\n");
    
    // Test 1: Type simple text
    println!("Test 1: Typing simple text into TextEdit");
    println!("  Please open TextEdit and create a new document...");
    std::thread::sleep(std::time::Duration::from_secs(3));
    
    match controller.type_text("TextEdit", "Hello, World!") {
        Ok(_) => println!("  ✅ Successfully typed simple text\n"),
        Err(e) => println!("  ❌ Failed: {}\n", e),
    }
    
    std::thread::sleep(std::time::Duration::from_secs(1));
    
    // Test 2: Type unicode and emojis
    println!("Test 2: Typing unicode and emojis");
    match controller.type_text("TextEdit", "\n🌟 Unicode test: café, naïve, 日本語 🎉") {
        Ok(_) => println!("  ✅ Successfully typed unicode text\n"),
        Err(e) => println!("  ❌ Failed: {}\n", e),
    }
    
    std::thread::sleep(std::time::Duration::from_secs(1));
    
    // Test 3: Type special characters
    println!("Test 3: Typing special characters");
    match controller.type_text("TextEdit", "\nSpecial: @#$%^&*()_+-=[]{}|;':,.<>?/") {
        Ok(_) => println!("  ✅ Successfully typed special characters\n"),
        Err(e) => println!("  ❌ Failed: {}\n", e),
    }
    
    println!("\n✨ Tests complete!");
    println!("\n💡 Now try with Things3:");
    println!("   1. Open Things3");
    println!("   2. Press Cmd+N to create a new task");
    println!("   3. Run: g3 --macax 'type \"🌟 My awesome task\" into Things'");
    
    Ok(())
}
