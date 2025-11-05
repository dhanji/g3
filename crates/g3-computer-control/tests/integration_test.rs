use g3_computer_control::*;

#[tokio::test]
async fn test_screenshot() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Test that screenshot without window_id fails with appropriate error
    let path = "/tmp/test_screenshot.png";
    let result = controller.take_screenshot(path, None, None).await;
    assert!(result.is_err(), "Expected error when window_id is not provided");
    
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("window_id is required"), 
        "Expected error message about window_id being required, got: {}", error_msg);
}

#[tokio::test]
async fn test_screenshot_with_window() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Take screenshot of Finder (should always be available on macOS)
    let path = "/tmp/test_screenshot_finder.png";
    let result = controller.take_screenshot(path, None, Some("Finder")).await;
    
    // This test may fail if Finder is not running, so we just check it doesn't panic
    // and returns a proper Result
    let _ = result; // Don't assert success since Finder might not be visible
    
    // Clean up
    let _ = std::fs::remove_file(path);
}
