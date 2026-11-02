//! Compaction Behavior Integration Tests
//!
//! CHARACTERIZATION: These tests verify the observable behavior of context
//! compaction through stable public interfaces.
//!
//! What these tests protect:
//! - Compaction configuration calculation (token caps, thinking mode)
//! - Summary message building from conversation history
//! - Compaction result handling (success/failure)
//!
//! What these tests intentionally do NOT assert:
//! - Internal implementation details of compaction
//! - Specific LLM responses (mocked at provider boundary)
//! - Exact token counts (only relative behavior)

use g3_core::compaction::{
    calculate_capped_summary_tokens, should_disable_thinking, build_summary_messages,
    CompactionResult, SUMMARY_MIN_TOKENS,
};
use g3_core::ContextWindow;
use g3_providers::{Message, MessageRole};

// =============================================================================
// Test: Token cap calculation for different providers
// =============================================================================

mod token_cap_calculation {
    use super::*;

    /// Test that Anthropic provider gets appropriate token caps
    #[test]
    fn test_anthropic_token_cap() {
        let config = g3_config::Config::default();
        
        // Large base tokens should be capped
        let capped = calculate_capped_summary_tokens(&config, "anthropic", 50000);
        assert!(capped <= 10000, "Anthropic should cap at 10000 by default, got {}", capped);
        assert!(capped >= SUMMARY_MIN_TOKENS, "Should respect minimum floor");
    }

    /// Test that unknown providers get conservative caps
    #[test]
    fn test_unknown_provider_token_cap() {
        let config = g3_config::Config::default();
        
        let capped = calculate_capped_summary_tokens(&config, "unknown_provider", 50000);
        assert!(capped <= 5000, "Unknown providers should cap at 5000, got {}", capped);
        assert!(capped >= SUMMARY_MIN_TOKENS, "Should respect minimum floor");
    }

    /// Test that small base tokens are preserved (not increased)
    #[test]
    fn test_small_base_tokens_preserved() {
        let config = g3_config::Config::default();
        
        // If base is already small, it should be preserved (but not below minimum)
        let capped = calculate_capped_summary_tokens(&config, "anthropic", 2000);
        assert_eq!(capped, 2000, "Small base tokens should be preserved");
    }

    /// Test minimum floor is enforced
    #[test]
    fn test_minimum_floor_enforced() {
        let config = g3_config::Config::default();
        
        // Even with very small base, minimum should be enforced
        let capped = calculate_capped_summary_tokens(&config, "anthropic", 100);
        assert_eq!(capped, SUMMARY_MIN_TOKENS, "Minimum floor should be enforced");
    }
}

// =============================================================================
// Test: Thinking mode disable logic
// =============================================================================

mod thinking_mode_disable {
    use super::*;

    /// Test that thinking mode is not disabled when no thinking config exists
    #[test]
    fn test_no_thinking_config_no_disable() {
        let config = g3_config::Config::default();
        
        // Without thinking config, should never disable
        let should_disable = should_disable_thinking(&config, "anthropic", 5000);
        assert!(!should_disable, "Should not disable thinking when no config exists");
    }

    /// Test that non-Anthropic providers don't trigger thinking disable
    #[test]
    fn test_non_anthropic_no_thinking_disable() {
        let config = g3_config::Config::default();
        
        // Non-Anthropic providers don't have thinking mode
        let should_disable = should_disable_thinking(&config, "gemini", 1000);
        assert!(!should_disable, "Non-Anthropic providers should not disable thinking");
    }
}

// =============================================================================
// Test: Summary message building
// =============================================================================

mod summary_message_building {
    use super::*;

    /// Test that summary messages are built correctly from conversation
    #[test]
    fn test_build_summary_messages_basic() {
        let mut context = ContextWindow::new(10000);
        
        // Add a simple conversation
        context.add_message(Message::new(
            MessageRole::System,
            "You are a helpful assistant.".to_string(),
        ));
        context.add_message(Message::new(
            MessageRole::User,
            "Hello, how are you?".to_string(),
        ));
        context.add_message(Message::new(
            MessageRole::Assistant,
            "I'm doing well, thank you!".to_string(),
        ));
        
        let messages = build_summary_messages(&context);
        
        // Should have exactly 2 messages: system prompt and user request
        assert_eq!(messages.len(), 2, "Should have system and user messages");
        
        // First should be system message for summarization
        assert!(matches!(messages[0].role, MessageRole::System));
        assert!(messages[0].content.contains("concise summaries"));
        
        // Second should be user message with conversation
        assert!(matches!(messages[1].role, MessageRole::User));
        assert!(messages[1].content.contains("Hello, how are you?"));
        assert!(messages[1].content.contains("I'm doing well"));
    }

    /// Test that empty conversation produces valid summary request
    #[test]
    fn test_build_summary_messages_empty_conversation() {
        let context = ContextWindow::new(10000);
        
        let messages = build_summary_messages(&context);
        
        // Should still produce valid structure
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0].role, MessageRole::System));
        assert!(matches!(messages[1].role, MessageRole::User));
    }

    /// Test that long conversations are included in summary request
    #[test]
    fn test_build_summary_messages_long_conversation() {
        let mut context = ContextWindow::new(100000);
        
        // Add many messages
        for i in 0..50 {
            context.add_message(Message::new(
                MessageRole::User,
                format!("User message number {}", i),
            ));
            context.add_message(Message::new(
                MessageRole::Assistant,
                format!("Assistant response number {}", i),
            ));
        }
        
        let messages = build_summary_messages(&context);
        
        // Should include all conversation content
        let user_content = &messages[1].content;
        assert!(user_content.contains("User message number 0"));
        assert!(user_content.contains("User message number 49"));
        assert!(user_content.contains("Assistant response number 49"));
    }
}

// =============================================================================
// Test: CompactionResult behavior
// =============================================================================

mod compaction_result {
    use super::*;

    /// Test success result creation
    #[test]
    fn test_success_result() {
        let result = CompactionResult::success(5000);
        
        assert!(result.success);
        assert_eq!(result.chars_saved, 5000);
        assert!(result.error.is_none());
    }

    /// Test failure result creation
    #[test]
    fn test_failure_result() {
        let result = CompactionResult::failure("API error".to_string());
        
        assert!(!result.success);
        assert_eq!(result.chars_saved, 0);
        assert_eq!(result.error, Some("API error".to_string()));
    }

    /// Test zero chars saved is valid success
    #[test]
    fn test_zero_chars_saved_success() {
        let result = CompactionResult::success(0);
        
        assert!(result.success);
        assert_eq!(result.chars_saved, 0);
    }
}
