use crate::ContextWindow;

/// Result of a task execution containing both the response and the context window
#[derive(Debug, Clone)]
pub struct TaskResult {
    /// The actual response content from the task execution
    pub response: String,
    /// The complete context window at the time of completion
    pub context_window: ContextWindow,
}

impl TaskResult {
    pub fn new(response: String, context_window: ContextWindow) -> Self {
        Self {
            response,
            context_window,
        }
    }

    /// Extract the final_output content from the response (for coach feedback in autonomous mode)
    /// This looks for the complete final_output content, not just the last block
    pub fn extract_final_output(&self) -> String {
        // Remove any timing information at the end
        let content_without_timing = if let Some(timing_pos) = self.response.rfind("\n⏱️") {
            &self.response[..timing_pos]
        } else {
            &self.response
        };

        // Look for the final_output marker pattern
        // The final_output content typically appears after the tool is called
        // and is the substantive content that follows
        
        // First, try to find if there's a clear final_output section
        // This would be the content after the last tool execution
        if let Some(final_output_pos) = content_without_timing.rfind("final_output") {
            // Find the content that follows the final_output call
            // Skip past the tool call line and any immediate formatting
            if let Some(content_start) = content_without_timing[final_output_pos..].find('\n') {
                let start_pos = final_output_pos + content_start + 1;
                let final_content = &content_without_timing[start_pos..];
                
                // Trim and return the complete content
                let trimmed = final_content.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
        
        // Fallback to the original extract_last_block behavior if we can't find final_output
        // This maintains backward compatibility
        self.extract_last_block()
    }

    /// Extract the last block from the response (for coach feedback in autonomous mode)
    /// This looks for the final_output content which is the last substantial block
    pub fn extract_last_block(&self) -> String {
        // Remove any timing information at the end
        let content_without_timing = if let Some(timing_pos) = self.response.rfind("\n⏱️") {
            &self.response[..timing_pos]
        } else {
            &self.response
        };
        
        // Split by double newlines to find the last substantial block
        let blocks: Vec<&str> = content_without_timing.split("\n\n").collect();
        
        // Find the last non-empty block that isn't just whitespace
        blocks.iter()
            .rev()
            .find(|block| !block.trim().is_empty())
            .map(|block| block.trim().to_string())
            .unwrap_or_else(|| {
                // Fallback: if we can't find a clear block, take the whole thing
                content_without_timing.trim().to_string()
            })
    }

    /// Check if the response contains an approval (for autonomous mode)
    pub fn is_approved(&self) -> bool {
        self.extract_final_output().contains("IMPLEMENTATION_APPROVED")
    }
}

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

    #[test]
    fn test_extract_final_output() {
        let context_window = ContextWindow::new(1000);
        
        // Test case 1: Response with final_output tool call
        let response_with_final_output = "Analyzing files...\n\nCalling final_output\n\nThis is the complete feedback\nwith multiple lines\nand important details\n\n⏱️ 2.3s".to_string();
        let result = TaskResult::new(response_with_final_output, context_window.clone());
        assert_eq!(result.extract_final_output(), "This is the complete feedback\nwith multiple lines\nand important details");
        
        // Test case 2: Response with IMPLEMENTATION_APPROVED in final_output
        let response_approved = "Review complete\n\nfinal_output called\n\nIMPLEMENTATION_APPROVED".to_string();
        let result = TaskResult::new(response_approved, context_window.clone());
        assert_eq!(result.extract_final_output(), "IMPLEMENTATION_APPROVED");
        assert!(result.is_approved());
        
        // Test case 3: Response with detailed feedback in final_output
        let response_feedback = "Checking implementation...\n\nfinal_output\n\nThe following issues need to be addressed:\n1. Missing error handling in main.rs\n2. Tests are not comprehensive\n3. Documentation needs improvement\n\nPlease fix these issues.".to_string();
        let result = TaskResult::new(response_feedback, context_window.clone());
        let extracted = result.extract_final_output();
        assert!(extracted.contains("The following issues need to be addressed:"));
        assert!(extracted.contains("1. Missing error handling"));
        assert!(extracted.contains("Please fix these issues."));
        assert!(!result.is_approved());
        
        // Test case 4: Response without final_output (fallback to extract_last_block)
        let response_no_final_output = "Some analysis\n\nFinal thoughts here".to_string();
        let result = TaskResult::new(response_no_final_output, context_window.clone());
        assert_eq!(result.extract_final_output(), "Final thoughts here");
        
        // Test case 5: Empty response
        let empty_response = "".to_string();
        let result = TaskResult::new(empty_response, context_window);
        assert_eq!(result.extract_final_output(), "");
    }
}
