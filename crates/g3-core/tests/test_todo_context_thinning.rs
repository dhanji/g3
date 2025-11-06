use g3_core::ContextWindow;
use g3_providers::{Message, MessageRole};
use serial_test::serial;

#[test]
#[serial]
fn test_todo_read_results_not_thinned() {
    let mut context = ContextWindow::new(10000);
    
    // Add a todo_read tool call
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: r#"{"tool": "todo_read", "args": {}}"#.to_string(),
    });
    
    // Add a large TODO result (> 500 chars)
    let large_todo_result = format!(
        "Tool result: 📝 TODO list:\n{}",
        "- [ ] Task with long description\n".repeat(50)
    );
    context.add_message(Message {
        role: MessageRole::User,
        content: large_todo_result.clone(),
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
    
    // Check that the TODO result was NOT thinned
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                // TODO result should still be large (not thinned)
                assert!(
                    msg.content.len() > 500,
                    "TODO result at index {} should not have been thinned. Content: {}",
                    i,
                    msg.content
                );
                assert!(
                    msg.content.contains("📝 TODO list:"),
                    "TODO result should still contain full content"
                );
            }
        }
    }
}

#[test]
#[serial]
fn test_todo_write_results_not_thinned() {
    let mut context = ContextWindow::new(10000);
    
    // Add a todo_write tool call
    let large_content = "- [ ] Task\n".repeat(100);
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: format!(r#"{{"tool": "todo_write", "args": {{"content": "{}"}}}}"#, large_content),
    });
    
    // Add a large TODO write result
    let large_todo_result = format!(
        "Tool result: ✅ TODO list updated ({} chars) and saved to todo.g3.md",
        large_content.len()
    );
    context.add_message(Message {
        role: MessageRole::User,
        content: large_todo_result.clone(),
    });
    
    // Add more messages
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
    
    // Check that the TODO write result was NOT thinned
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                // Should not be replaced with file reference
                assert!(
                    !msg.content.contains("Tool result saved to"),
                    "TODO write result should not be thinned to file reference"
                );
                assert!(
                    msg.content.contains("todo.g3.md"),
                    "TODO write result should still contain todo.g3.md reference"
                );
            }
        }
    }
}

#[test]
#[serial]
fn test_non_todo_results_still_thinned() {
    let mut context = ContextWindow::new(10000);
    
    // Add a non-TODO tool call (e.g., read_file)
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: r#"{"tool": "read_file", "args": {"file_path": "test.txt"}}"#.to_string(),
    });
    
    // Add a large read_file result (> 500 chars)
    let large_result = format!("Tool result: {}", "x".repeat(1500));
    context.add_message(Message {
        role: MessageRole::User,
        content: large_result,
    });
    
    // Add more messages
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
    
    // Should have thinned the non-TODO result
    assert!(
        summary.contains("1 tool result") || summary.contains("chars saved"),
        "Non-TODO results should be thinned"
    );
    
    // Check that the result was actually thinned
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                // Should be replaced with file reference
                assert!(
                    msg.content.contains("Tool result saved to") || msg.content.len() < 1000,
                    "Non-TODO result should have been thinned"
                );
            }
        }
    }
}

#[test]
#[serial]
fn test_todo_read_with_spaces_in_tool_name() {
    let mut context = ContextWindow::new(10000);
    
    // Add a todo_read tool call with spaces (JSON formatting variation)
    context.add_message(Message {
        role: MessageRole::Assistant,
        content: r#"{"tool": "todo_read", "args": {}}"#.to_string(),
    });
    
    // Add a large TODO result
    let large_todo_result = format!(
        "Tool result: 📝 TODO list:\n{}",
        "- [ ] Task\n".repeat(50)
    );
    context.add_message(Message {
        role: MessageRole::User,
        content: large_todo_result.clone(),
    });
    
    // Add more messages
    for i in 0..6 {
        context.add_message(Message {
            role: MessageRole::Assistant,
            content: format!("Response {}", i),
        });
    }
    
    // Trigger thinning
    context.used_tokens = 5000;
    let (_summary, _chars_saved) = context.thin_context();
    
    // Verify TODO result was not thinned
    let first_third_end = context.conversation_history.len() / 3;
    for i in 0..first_third_end {
        if let Some(msg) = context.conversation_history.get(i) {
            if matches!(msg.role, MessageRole::User) && msg.content.starts_with("Tool result:") {
                assert!(
                    msg.content.len() > 500,
                    "TODO result should not be thinned even with space in JSON"
                );
            }
        }
    }
}
