use g3_computer_control::{create_controller, ComputerController};

#[tokio::main]
async fn main() {
    println!("Testing screenshot with permission prompt...");
    
    let controller = create_controller().expect("Failed to create controller");
    
    match controller.take_screenshot("/tmp/test_with_prompt.png", None, None).await {
        Ok(_) => {
            println!("\n✅ Screenshot saved to /tmp/test_with_prompt.png");
            println!("Opening screenshot...");
            let _ = std::process::Command::new("open")
                .arg("/tmp/test_with_prompt.png")
                .spawn();
        }
        Err(e) => {
            println!("❌ Screenshot failed: {}", e);
        }
    }
}
