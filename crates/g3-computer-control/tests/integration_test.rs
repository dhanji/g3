use g3_computer_control::*;

#[tokio::test]
async fn test_mouse_movement() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Move mouse to center of screen (assuming 1920x1080)
    let result = controller.move_mouse(960, 540).await;
    assert!(result.is_ok(), "Failed to move mouse: {:?}", result.err());
}

#[tokio::test]
async fn test_typing() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Type some text
    let result = controller.type_text("Hello, World!").await;
    assert!(result.is_ok(), "Failed to type text: {:?}", result.err());
}

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

#[tokio::test]
async fn test_click() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Click at a safe location
    let result = controller.click(types::MouseButton::Left).await;
    assert!(result.is_ok(), "Failed to click: {:?}", result.err());
}

#[tokio::test]
async fn test_double_click() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Double click
    let result = controller.double_click(types::MouseButton::Left).await;
    assert!(result.is_ok(), "Failed to double click: {:?}", result.err());
}

#[tokio::test]
async fn test_press_key() {
    let controller = create_controller().expect("Failed to create controller");
    
    // Press escape key
    let result = controller.press_key("escape").await;
    assert!(result.is_ok(), "Failed to press key: {:?}", result.err());
}
