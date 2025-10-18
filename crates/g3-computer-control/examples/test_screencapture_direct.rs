use std::process::Command;

fn main() {
    let path = "/tmp/rust_screencapture_test.png";
    
    println!("Testing screencapture command from Rust...");
    
    let mut cmd = Command::new("screencapture");
    cmd.arg("-x"); // No sound
    cmd.arg(path);
    
    println!("Command: {:?}", cmd);
    
    match cmd.output() {
        Ok(output) => {
            println!("Exit status: {}", output.status);
            println!("Stdout: {}", String::from_utf8_lossy(&output.stdout));
            println!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
            
            if output.status.success() {
                println!("\n✅ Screenshot saved to: {}", path);
                
                // Check file exists and size
                if let Ok(metadata) = std::fs::metadata(path) {
                    println!("File size: {} bytes ({:.1} MB)", metadata.len(), metadata.len() as f64 / 1_000_000.0);
                }
                
                // Open it
                let _ = Command::new("open").arg(path).spawn();
                println!("\nOpened screenshot - please verify it looks correct!");
            } else {
                println!("\n❌ Screenshot failed!");
            }
        }
        Err(e) => {
            println!("❌ Failed to execute screencapture: {}", e);
        }
    }
}
