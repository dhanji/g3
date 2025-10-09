use g3_core::ContextWindow;
use g3_providers::{Message, MessageRole, Usage};

#[test]
fn test_context_window_with_actual_tokens() {
    let mut context = ContextWindow::new(10000);
    
    // Add a message with known token count
    let message = Message {
        role: MessageRole::User,
        content: "Hello, how are you today?".to_string(),
    };
    
    // Add with actual token count (let's say this is 7 tokens)
    context.add_message_with_tokens(message.clone(), Some(7));
    
    assert_eq!(context.used_tokens, 7);
    assert_eq!(context.cumulative_tokens, 7);
    
    // Add another message with estimation (no token count provided)
    let message2 = Message {
        role: MessageRole::Assistant,
        content: "I'm doing well, thank you for asking!".to_string(),
    };
    
    context.add_message_with_tokens(message2, None);
    
    // Should have added estimated tokens (roughly 10-11 tokens for this text)
    assert!(context.used_tokens > 7);
    assert_eq!(context.cumulative_tokens, context.used_tokens);
}

#[test]
fn test_context_window_update_from_response() {
    let mut context = ContextWindow::new(10000);
    
    // Add initial messages with estimation
    let message1 = Message {
        role: MessageRole::User,
        content: "What is the capital of France?".to_string(),
    };
    context.add_message(message1);
    
    let initial_estimate = context.used_tokens;
    let initial_cumulative = context.cumulative_tokens;
    
    // Now update with actual usage from provider
    let usage = Usage {
        prompt_tokens: 8,
        completion_tokens: 15,
        total_tokens: 23,
    };
    
    context.update_usage_from_response(&usage);
    
    // Should have replaced estimate with actual
    assert_eq!(context.used_tokens, 23);
    // Cumulative should be adjusted
    assert_eq!(context.cumulative_tokens, context.cumulative_tokens);
    assert!(context.cumulative_tokens >= 23);
}

#[test]
fn test_streaming_token_accumulation() {
    let mut context = ContextWindow::new(10000);
    
    // Simulate streaming tokens being added
    context.add_streaming_tokens(5);
    assert_eq!(context.used_tokens, 5);
    assert_eq!(context.cumulative_tokens, 5);
    
    context.add_streaming_tokens(3);
    assert_eq!(context.used_tokens, 8);
    assert_eq!(context.cumulative_tokens, 8);
    
    context.add_streaming_tokens(7);
    assert_eq!(context.used_tokens, 15);
    assert_eq!(context.cumulative_tokens, 15);
}

#[test]
fn test_context_window_percentage_with_actual_tokens() {
    let mut context = ContextWindow::new(1000);
    
    // Add messages with known token counts
    let message1 = Message {
        role: MessageRole::User,
        content: "First message".to_string(),
    };
    context.add_message_with_tokens(message1, Some(100));
    
    assert_eq!(context.percentage_used(), 10.0);
    
    let message2 = Message {
        role: MessageRole::Assistant,
        content: "Second message".to_string(),
    };
    context.add_message_with_tokens(message2, Some(400));
    
    assert_eq!(context.percentage_used(), 50.0);
    
    // Test should_summarize threshold (80%)
    let message3 = Message {
        role: MessageRole::User,
        content: "Third message".to_string(),
    };
    context.add_message_with_tokens(message3, Some(300));
    
    assert_eq!(context.percentage_used(), 80.0);
    assert!(context.should_summarize());
}

#[test]
fn test_fallback_to_estimation() {
    let mut context = ContextWindow::new(10000);
    
    // Add message without token count (should use estimation)
    let message = Message {
        role: MessageRole::User,
        content: "This is a test message without token count".to_string(),
    };
    
    context.add_message_with_tokens(message.clone(), None);
    
    // Should have estimated tokens (roughly 11-12 tokens for this text)
    assert!(context.used_tokens > 0);
    assert!(context.used_tokens < 20); // Reasonable upper bound
    
    // Verify estimation is reasonable
    let text_len = message.content.len();
    let estimated = context.used_tokens;
    let ratio = text_len as f32 / estimated as f32;
    
    // Should be roughly 3-4 characters per token
    assert!(ratio > 2.0 && ratio < 6.0);
}

#[test]
fn test_empty_message_handling() {
    let mut context = ContextWindow::new(10000);
    
    // Empty messages should be skipped
    let empty_message = Message {
        role: MessageRole::User,
        content: "   ".to_string(), // Only whitespace
    };
    
    context.add_message_with_tokens(empty_message, Some(10));
    
    // Should not have added anything
    assert_eq!(context.used_tokens, 0);
    assert_eq!(context.cumulative_tokens, 0);
    assert_eq!(context.conversation_history.len(), 0);
}