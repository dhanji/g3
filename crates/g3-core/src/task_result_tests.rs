#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_last_block() {
        // Test case 1: Response with timing info
        let context_window = ContextWindow::new(1000);
        let response_with_timing = "Some initial content\n\nFinal block content\n\n⏱️ 2.3s | 💭 1.2s".to_string();
        let result = TaskResult::new(response_with_timing, context_window.clone());
        assert_eq!(result.extract_last_block(), "Final block content");
        
        // Test case 2: Response without timing
        let response_no_timing = "Some initial content\n\nFinal block content".to_string();
        let result = TaskResult::new(response_no_timing, context_window.clone());
        assert_eq!(result.extract_last_block(), "Final block content");
        
        // Test case 3: Response with IMPLEMENTATION_APPROVED
        let response_approved = "Some content\n\nIMPLEMENTATION_APPROVED".to_string();
        let result = TaskResult::new(response_approved, context_window.clone());
        assert!(result.is_approved());
        
        // Test case 4: Response without approval
        let response_not_approved = "Some content\n\nNeeds more work".to_string();
        let result = TaskResult::new(response_not_approved, context_window);
        assert!(!result.is_approved());
    }

    #[test]
    fn test_extract_last_block_edge_cases() {
        let context_window = ContextWindow::new(1000);
        
        // Test empty response
        let empty_response = "".to_string();
        let result = TaskResult::new(empty_response, context_window.clone());
        assert_eq!(result.extract_last_block(), "");
        
        // Test single block
        let single_block = "Just one block".to_string();
        let result = TaskResult::new(single_block, context_window.clone());
        assert_eq!(result.extract_last_block(), "Just one block");
        
        // Test multiple empty blocks
        let multiple_empty = "\n\n\n\nSome content\n\n\n\n".to_string();
        let result = TaskResult::new(multiple_empty, context_window);
        assert_eq!(result.extract_last_block(), "Some content");
    }
}
