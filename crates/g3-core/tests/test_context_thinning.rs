use g3_core::ContextWindow;
use g3_providers::{Message, MessageRole};

#[test]
fn test_thinning_thresholds() {
    let mut context = ContextWindow::new(10000);
    
    // At 0%, should not thin
    assert!(!context.should_thin());
    
    // Simulate reaching 50% usage
    context.used_tokens = 5000;
    assert!(context.should_thin());
    
    // After thinning at 50%, should not thin again until next threshold
    context.last_thinning_percentage = 50;
    assert!(!context.should_thin());
    
    // At 60%, should thin again
    context.used_tokens = 6000;
    assert!(context.should_thin());
    
    // After thinning at 60%, should not thin
    context.last_thinning_percentage = 60;
    assert!(!context.should_thin());
    
    // At 70%, should thin
    context.used_tokens = 7000;
    assert!(context.should_thin());
    
    // At 80%, should thin
    context.last_thinning_percentage = 70;
    context.used_tokens = 8000;
    assert!(context.should_thin());
    
    // After 80%, should not thin (compaction takes over)
    context.last_thinning_percentage = 80;
    context.used_tokens = 8500;
    assert!(!context.should_thin());
}

#[test]
fn test_thin_context_basic() {
    let mut context = ContextWindow::new(10000);
    
    // Add some messages to the first third
    for i in 0..9 {
        if i % 2 == 0 {
            context.add_message(Message {
                role: MessageRole::Assistant,
                content: format!("Assistant message {}", i),
            });
        } else {
            // Add tool results with varying sizes
            let content = if i == 1 {
                // Large tool result (> 1000 chars)
                format!("Tool result: {}", "x".repeat(1500))
            } else if i == 3 {
                // Another large tool result
                format!("Tool result: {}", "y".repeat(2000))
            } else {
                // Small tool result (< 1000 chars)
                format!("Tool result: small result {}", i)
            };
            
            context.add_message(Message {
                role: MessageRole::User,
                content,
            });
        }
    }
    
    // Trigger thinning at 50%
    context.used_tokens = 5000;
    let (summary, _chars_saved) = context.thin_context();
    
    println!("Thinning summary: {}", summary);
    
    // Should have thinned at least 1 large tool result in the first third
    assert!(summary.contains("1 tool result"), "Summary was: {}", summary);
    assert!(summary.contains("50%"));
    
    // Check that the large tool results were replaced
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                if msg.content.len() > 1000 {
                    panic!("Found un-thinned large tool result at index {}", i);
                }
            }
        }
    }
}

#[test]
fn test_thin_write_file_tool_calls() {
    let mut context = ContextWindow::new(10000);
    
    // Add some messages including a write_file tool call with large content
    context.add_message(Message {
        role: MessageRole::User,
        content: "Please create a large file".to_string(),
    });
    
    // Add an assistant message with a write_file tool call containing large content
    let large_content = "x".repeat(1500);
    let tool_call_json = format!(
        r#"{{"tool": "write_file", "args": {{"file_path": "test.txt", "content": "{}"}}}}"#,
        large_content
    );
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: format!("I'll create that file.\n\n{}", tool_call_json),
    });
    
    context.add_message(Message {
        role: MessageRole::User,
        content: "Tool result: ✅ Successfully wrote 1500 lines".to_string(),
    });
    
    // Add more messages to ensure we have enough for "first third" logic
    for i in 0..6 {
        context.add_message(Message {
            role: MessageRole::Assistant,
            content: format!("Response {}", i),
        });
    }
    
    // Trigger thinning at 50%
    context.used_tokens = 5000;
    let (summary, _chars_saved) = context.thin_context();
    
    println!("Thinning summary: {}", summary);
    
    // Should have thinned the write_file tool call
    assert!(summary.contains("tool call") || summary.contains("chars saved"));
    
    // Check that the large content was replaced with a file reference
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::Assistant) && msg.content.contains("write_file") {
                // The content should now reference an external file
                assert!(msg.content.contains("<content saved to"));
                assert!(!msg.content.contains(&large_content));
            }
        }
    }
}

#[test]
fn test_thin_str_replace_tool_calls() {
    let mut context = ContextWindow::new(10000);
    
    // Add some messages including a str_replace tool call with large diff
    context.add_message(Message {
        role: MessageRole::User,
        content: "Please update the file".to_string(),
    });
    
    // Add an assistant message with a str_replace tool call containing large diff
    let large_diff = format!("--- old\n{}\n+++ new\n{}", "-old line\n".repeat(100), "+new line\n".repeat(100));
    let tool_call_json = format!(
        r#"{{"tool": "str_replace", "args": {{"file_path": "test.txt", "diff": "{}"}}}}"#,
        large_diff.replace('\n', "\\n")
    );
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: format!("I'll update that file.\n\n{}", tool_call_json),
    });
    
    context.add_message(Message {
        role: MessageRole::User,
        content: "Tool result: ✅ applied unified diff".to_string(),
    });
    
    // Add more messages to ensure we have enough for "first third" logic
    for i in 0..6 {
        context.add_message(Message {
            role: MessageRole::Assistant,
            content: format!("Response {}", i),
        });
    }
    
    // Trigger thinning at 50%
    context.used_tokens = 5000;
    let (summary, _chars_saved) = context.thin_context();
    
    println!("Thinning summary: {}", summary);
    
    // Should have thinned the str_replace tool call
    assert!(summary.contains("tool call") || summary.contains("chars saved"));
    
    // Check that the large diff was replaced with a file reference
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::Assistant) && msg.content.contains("str_replace") {
                // The diff should now reference an external file
                assert!(msg.content.contains("<diff saved to"));
                // Should not contain the large diff content
                assert!(!msg.content.contains("old line"));
            }
        }
    }
}

#[test]
fn test_thin_context_no_large_results() {
    let mut context = ContextWindow::new(10000);
    
    // Add only small messages
    for i in 0..9 {
        context.add_message(Message {
            role: MessageRole::User,
            content: format!("Tool result: small {}", i),
        });
    }
    
    context.used_tokens = 5000;
    let (summary, _chars_saved) = context.thin_context();
    
    // Should report no large results found
    assert!(summary.contains("no large tool results or tool calls found"));
}

#[test]
fn test_thin_context_only_affects_first_third() {
    let mut context = ContextWindow::new(10000);
    
    // Add 12 messages (first third = 4 messages)
    for i in 0..12 {
        let content = if i % 2 == 1 {
            // All odd indices are large tool results
            format!("Tool result: {}", "x".repeat(1500))
        } else {
            format!("Assistant message {}", i)
        };
        
        let role = if i % 2 == 1 {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        
        context.add_message(Message { role, content });
    }
    
    context.used_tokens = 5000;
    let (summary, _chars_saved) = context.thin_context();
    
    // First third is 4 messages (indices 0-3), so only indices 1 and 3 should be thinned
    // That's 2 tool results
    assert!(summary.contains("2 tool results"));
    
    // Check that messages after the first third are NOT thinned
    let first_third_end = context.conversation_history.len() / 3;
    for i in first_third_end..context.conversation_history.len() {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                // These should still be large (not thinned)
                if i % 2 == 1 {
                    assert!(msg.content.len() > 1000, 
                        "Message at index {} should not have been thinned", i);
                }
            }
        }
    }
}
