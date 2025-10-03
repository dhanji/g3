//! Test for the new centralized retry mechanism

use g3_providers::{
    AnthropicProvider, DatabricksProvider, LLMProvider, CompletionRequest, Message, MessageRole,
    RetryConfig, RetryProvider,
};
use std::env;

#[tokio::test]
#[ignore] // This test requires real API keys, so it's ignored by default
async fn test_retry_wrapper_with_anthropic() {
    // This test demonstrates using the RetryProvider wrapper
    // Run with: cargo test --test centralized_retry_test -- --ignored --nocapture
    
    let api_key = env::var("ANTHROPIC_API_KEY")
        .expect("ANTHROPIC_API_KEY environment variable must be set for this test");
    
    // Note: tracing can be initialized if needed for debugging
    
    // Create base provider without retry
    let base_provider = AnthropicProvider::new(
        api_key,
        Some("claude-3-5-sonnet-20241022".to_string()),
        Some(1000),
        Some(0.7),
    ).expect("Failed to create provider");
    
    // Wrap with retry provider using custom configuration
    let retry_config = RetryConfig::new(
        5,      // max_retries
        500,    // initial_delay_ms
        15000,  // max_delay_ms
        1.5,    // multiplier
    );
    
    let provider = RetryProvider::with_config(base_provider, retry_config);
    
    // Create a request
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: MessageRole::User,
                content: "Say hello in one word.".to_string(),
            },
        ],
        max_tokens: Some(10),
        temperature: Some(0.7),
        stream: false,
        tools: None,
    };
    
    // Test completion with retry wrapper
    match provider.complete(request.clone()).await {
        Ok(response) => {
            println!("✅ Completion with retry wrapper succeeded");
            println!("Response: {}", response.content);
            println!("Model: {}", response.model);
        }
        Err(e) => {
            println!("❌ Completion failed after retries: {}", e);
        }
    }
}

#[tokio::test]
async fn test_retry_config_presets() {
    // Test the different retry configuration presets
    
    let default_config = RetryConfig::default();
    assert_eq!(default_config.max_retries, 3);
    assert_eq!(default_config.initial_delay_ms, 1000);
    assert_eq!(default_config.max_delay_ms, 10000);
    assert_eq!(default_config.multiplier, 2.0);
    println!("✅ Default config: {:?}", default_config);
    
    let no_retry_config = RetryConfig::no_retry();
    assert_eq!(no_retry_config.max_retries, 0);
    println!("✅ No retry config: {:?}", no_retry_config);
    
    let aggressive_config = RetryConfig::aggressive();
    assert_eq!(aggressive_config.max_retries, 5);
    assert_eq!(aggressive_config.initial_delay_ms, 500);
    assert_eq!(aggressive_config.max_delay_ms, 30000);
    assert_eq!(aggressive_config.multiplier, 1.5);
    println!("✅ Aggressive config: {:?}", aggressive_config);
    
    let gentle_config = RetryConfig::gentle();
    assert_eq!(gentle_config.max_retries, 2);
    assert_eq!(gentle_config.initial_delay_ms, 2000);
    assert_eq!(gentle_config.max_delay_ms, 5000);
    assert_eq!(gentle_config.multiplier, 2.0);
    println!("✅ Gentle config: {:?}", gentle_config);
}

#[tokio::test]
async fn test_provider_with_custom_retry_config() {
    // Test that providers can have their own retry configuration
    
    // Create Anthropic provider with custom retry config
    let anthropic = AnthropicProvider::new(
        "test-key".to_string(),
        None,
        None,
        None,
    )
    .expect("Failed to create provider")
    .with_retry_config(RetryConfig::aggressive());
    
    assert_eq!(anthropic.retry_config().max_retries, 5);
    println!("✅ Anthropic provider with aggressive retry config");
    
    // Test that we can also use the wrapper for double retry protection
    let double_retry = RetryProvider::with_config(
        anthropic,
        RetryConfig::gentle(),
    );
    
    // The wrapper will use gentle config, while the inner provider uses aggressive
    assert_eq!(double_retry.config().max_retries, 2);
    println!("✅ Double retry protection configured");
}

#[tokio::test]
#[ignore]
async fn test_databricks_with_retry() {
    // Test Databricks provider with the new retry functionality
    // Run with: cargo test --test centralized_retry_test test_databricks_with_retry -- --ignored --nocapture
    
    let host = env::var("DATABRICKS_HOST")
        .expect("DATABRICKS_HOST environment variable must be set for this test");
    let token = env::var("DATABRICKS_TOKEN")
        .expect("DATABRICKS_TOKEN environment variable must be set for this test");
    
    // Note: tracing can be initialized if needed for debugging
    
    // Create Databricks provider with custom retry config
    let provider = DatabricksProvider::from_token(
        host,
        token,
        "databricks-meta-llama-3-3-70b-instruct".to_string(),
        None,
        None,
    )
    .expect("Failed to create provider")
    .with_retry_config(RetryConfig::default());
    
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: MessageRole::User,
                content: "What is 2+2?".to_string(),
            },
        ],
        max_tokens: Some(10),
        temperature: Some(0.1),
        stream: false,
        tools: None,
    };
    
    match provider.complete(request).await {
        Ok(response) => {
            println!("✅ Databricks completion with retry succeeded");
            println!("Response: {}", response.content);
        }
        Err(e) => {
            println!("❌ Databricks completion failed: {}", e);
        }
    }
}