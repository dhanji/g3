use g3_computer_control::*;

#[tokio::test]
async fn test_screenshot() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Take screenshot
    let path = "/tmp/test_screenshot.png";
    let result = controller.take_screenshot(path, None, None).await;
    assert!(result.is_ok(), "Failed to take screenshot: {:?}", result.err());
    
    // Verify file exists
    assert!(std::path::Path::new(path).exists(), "Screenshot file was not created");
    
    // Clean up
    let _ = std::fs::remove_file(path);
}
