use crate::{ContextWindow, TaskResult};
use g3_providers::{Message, MessageRole};
use std::sync::Arc;

#[test]
fn test_task_result_basic_functionality() {
    // Create a context window with some messages
    let mut context = ContextWindow::new(10000);
    context.add_message(Message {
        role: MessageRole::User,
        content: "Test message 1".to_string(),
    });
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: "Response 1".to_string(),
    });
    
    // Create a TaskResult
    let response = "This is the response\n\nFinal output block".to_string();
    let result = TaskResult::new(response.clone(), context.clone());
    
    // Test basic properties
    assert_eq!(result.response, response);
    assert_eq!(result.context_window.conversation_history.len(), 2);
    assert_eq!(result.context_window.total_tokens, 10000);
}

#[test]
fn test_extract_last_block_various_formats() {
    let context = ContextWindow::new(1000);
    
    // Test 1: Standard format with multiple blocks
    let response1 = "First block\n\nSecond block\n\nThird block".to_string();
    let result1 = TaskResult::new(response1, context.clone());
    assert_eq!(result1.extract_last_block(), "Third block");
    
    // Test 2: With timing information
    let response2 = "Content\n\nFinal block\n\n⏱️ 2.3s | 💭 1.2s".to_string();
    let result2 = TaskResult::new(response2, context.clone());
    assert_eq!(result2.extract_last_block(), "Final block");
    
    // Test 3: Single line response
    let response3 = "Single line response".to_string();
    let result3 = TaskResult::new(response3, context.clone());
    assert_eq!(result3.extract_last_block(), "Single line response");
    
    // Test 4: Empty response
    let response4 = "".to_string();
    let result4 = TaskResult::new(response4, context.clone());
    assert_eq!(result4.extract_last_block(), "");
    
    // Test 5: Only whitespace
    let response5 = "\n\n\n   \n\n".to_string();
    let result5 = TaskResult::new(response5, context.clone());
    assert_eq!(result5.extract_last_block(), "");
    
    // Test 6: Multiple blocks with empty ones
    let response6 = "First\n\n\n\n\n\nLast block here".to_string();
    let result6 = TaskResult::new(response6, context.clone());
    assert_eq!(result6.extract_last_block(), "Last block here");
}

#[test]
fn test_is_approved_detection() {
    let context = ContextWindow::new(1000);
    
    // Test approved cases
    let approved_responses = vec![
        "Analysis complete\n\nIMPLEMENTATION_APPROVED",
        "Some content\n\nThe implementation is good. IMPLEMENTATION_APPROVED",
        "IMPLEMENTATION_APPROVED",
        "Review done\n\n✅ IMPLEMENTATION_APPROVED - All tests pass",
    ];
    
    for response in approved_responses {
        let result = TaskResult::new(response.to_string(), context.clone());
        assert!(result.is_approved(), "Failed to detect approval in: {}", response);
    }
    
    // Test not approved cases
    let not_approved_responses = vec![
        "Needs more work",
        "Implementation needs fixes",
        "IMPLEMENTATION_REJECTED",
        "Almost there but not APPROVED",
        "",
    ];
    
    for response in not_approved_responses {
        let result = TaskResult::new(response.to_string(), context.clone());
        assert!(!result.is_approved(), "Incorrectly detected approval in: {}", response);
    }
}

#[test]
fn test_context_window_preservation() {
    // Create a context window with specific state
    let mut context = ContextWindow::new(5000);
    context.used_tokens = 1234;
    
    // Add some messages
    for i in 0..5 {
        context.add_message(Message {
            role: if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
            content: format!("Message {}", i),
        });
    }
    
    // Create TaskResult
    let result = TaskResult::new("Response".to_string(), context.clone());
    
    // Verify context is preserved
    assert_eq!(result.context_window.total_tokens, 5000);
    assert!(result.context_window.used_tokens > 1234); // Should have increased
    assert_eq!(result.context_window.conversation_history.len(), 5);
    
    // Verify messages are preserved correctly
    for i in 0..5 {
        let is_user = matches!(result.context_window.conversation_history[i].role, MessageRole::User);
        let expected_is_user = i % 2 == 0;
        assert_eq!(is_user, expected_is_user, "Message {} has wrong role", i);
        assert_eq!(result.context_window.conversation_history[i].content, format!("Message {}", i));
    }
}

#[test]
fn test_coach_feedback_extraction_scenarios() {
    let context = ContextWindow::new(1000);
    
    // Scenario 1: Coach feedback with file operations and analysis
    let coach_response = r#"Reading file: src/main.rs
📄 File content (23 lines):
fn main() {
    println!("Hello");
}

Analyzing implementation...

The implementation needs the following fixes:
1. Add error handling
2. Implement missing functions
3. Add tests"#;
    
    let result = TaskResult::new(coach_response.to_string(), context.clone());
    let feedback = result.extract_last_block();
    assert!(feedback.contains("Add error handling"));
    assert!(feedback.contains("Implement missing functions"));
    assert!(feedback.contains("Add tests"));
    
    // Scenario 2: Coach approval
    let approval_response = r#"Checking compilation...
✅ Build successful

Running tests...
✅ All tests pass

IMPLEMENTATION_APPROVED"#;
    
    let result = TaskResult::new(approval_response.to_string(), context.clone());
    assert!(result.is_approved());
    assert_eq!(result.extract_last_block(), "IMPLEMENTATION_APPROVED");
    
    // Scenario 3: Complex feedback with timing
    let complex_response = r#"Tool execution log...

Analysis complete.

The following issues were found:
- Memory leak in process_data()
- Missing input validation

⏱️ 5.2s | 💭 2.1s"#;
    
    let result = TaskResult::new(complex_response.to_string(), context.clone());
    let feedback = result.extract_last_block();
    assert!(feedback.contains("Memory leak"));
    assert!(feedback.contains("Missing input validation"));
    assert!(!feedback.contains("⏱️")); // Timing should be stripped
}

#[test]
fn test_edge_cases_and_special_characters() {
    let context = ContextWindow::new(1000);
    
    // Test with special characters and emojis
    let response_with_emojis = "First part 🚀\n\n✅ Final part with emojis 🎉".to_string();
    let result = TaskResult::new(response_with_emojis, context.clone());
    assert_eq!(result.extract_last_block(), "✅ Final part with emojis 🎉");
    
    // Test with code blocks
    let response_with_code = "Explanation\n\n```rust\nfn main() {}\n```\n\nFinal comment".to_string();
    let result = TaskResult::new(response_with_code, context.clone());
    assert_eq!(result.extract_last_block(), "Final comment");
    
    // Test with mixed newlines
    let mixed_newlines = "Part 1\r\n\r\nPart 2\n\nPart 3".to_string();
    let result = TaskResult::new(mixed_newlines, context.clone());
    assert_eq!(result.extract_last_block(), "Part 3");
}

#[test]
fn test_large_response_handling() {
    let context = ContextWindow::new(100000);
    
    // Create a large response
    let mut large_response = String::new();
    for i in 0..100 {
        large_response.push_str(&format!("Block {} with some content\n\n", i));
    }
    large_response.push_str("This is the final block after 100 other blocks");
    
    let result = TaskResult::new(large_response, context);
    assert_eq!(result.extract_last_block(), "This is the final block after 100 other blocks");
}

#[test]
fn test_concurrent_access() {
    use std::thread;
    
    let context = ContextWindow::new(1000);
    let result = Arc::new(TaskResult::new(
        "Concurrent test\n\nFinal block".to_string(),
        context,
    ));
    
    let mut handles = vec![];
    
    // Spawn multiple threads to access the TaskResult
    for _ in 0..10 {
        let result_clone = Arc::clone(&result);
        let handle = thread::spawn(move || {
            // Each thread extracts the last block
            let block = result_clone.extract_last_block();
            assert_eq!(block, "Final block");
            
            // Check approval status
            assert!(!result_clone.is_approved());
        });
        handles.push(handle);
    }
    
    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }
}

