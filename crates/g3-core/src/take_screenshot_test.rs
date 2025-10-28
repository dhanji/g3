// Test to verify take_screenshot requires window_id

#[cfg(test)]
mod take_screenshot_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_take_screenshot_requires_window_id() {
        // Create a tool call without window_id
        let tool_call = ToolCall {
            tool: "take_screenshot".to_string(),
            args: json!({
                "path": "test.png"
            }),
        };

        // Verify that window_id is missing
        assert!(tool_call.args.get("window_id").is_none());
    }

    #[test]
    fn test_take_screenshot_with_window_id() {
        // Create a tool call with window_id
        let tool_call = ToolCall {
            tool: "take_screenshot".to_string(),
            args: json!({
                "path": "test.png",
                "window_id": "Safari"
            }),
        };

        // Verify that window_id is present
        assert!(tool_call.args.get("window_id").is_some());
        assert_eq!(tool_call.args.get("window_id").unwrap().as_str().unwrap(), "Safari");
    }
}
