//! Integration tests for OpenRouter provider
//!
//! These tests verify OpenRouter provider functionality including basic API integration,
//! streaming support, and provider routing features.

use g3_providers::{
    CompletionRequest, LLMProvider, Message, MessageRole, OpenRouterProvider, ProviderPreferences,
    Tool,
};
use serde_json::json;
use std::env;
use tokio_stream::StreamExt;

/// Helper function to get API key from environment or skip test
fn get_api_key_or_skip() -> Option<String> {
    env::var("OPENROUTER_API_KEY").ok()
}

#[tokio::test]
async fn test_openrouter_basic_completion() {
    let Some(api_key) = get_api_key_or_skip() else {
        println!("Skipping test: OPENROUTER_API_KEY not set");
        return;
    };

    let provider = OpenRouterProvider::new(
        api_key,
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(100),
        Some(0.7),
    )
    .expect("Failed to create OpenRouter provider");

    let request = CompletionRequest {
        messages: vec![Message::new(
            MessageRole::User,
            "Say 'test successful' and nothing else.".to_string(),
        )],
        max_tokens: Some(50),
        temperature: Some(0.7),
        stream: false,
        tools: None,
        disable_thinking: false,
    };

    let response = provider
        .complete(request)
        .await
        .expect("Completion request failed");

    println!("Response: {}", response.content);
    assert!(!response.content.is_empty(), "Response should not be empty");
    assert!(
        response.usage.total_tokens > 0,
        "Token usage should be tracked"
    );
}

#[tokio::test]
async fn test_openrouter_streaming() {
    let Some(api_key) = get_api_key_or_skip() else {
        println!("Skipping test: OPENROUTER_API_KEY not set");
        return;
    };

    let provider = OpenRouterProvider::new(
        api_key,
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(100),
        Some(0.7),
    )
    .expect("Failed to create OpenRouter provider");

    let request = CompletionRequest {
        messages: vec![Message::new(
            MessageRole::User,
            "Count from 1 to 5.".to_string(),
        )],
        max_tokens: Some(50),
        temperature: Some(0.7),
        stream: true,
        tools: None,
        disable_thinking: false,
    };

    let mut stream = provider
        .stream(request)
        .await
        .expect("Streaming request failed");

    let mut accumulated_content = String::new();
    let mut chunk_count = 0;
    let mut final_usage = None;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                chunk_count += 1;
                accumulated_content.push_str(&chunk.content);

                if chunk.finished {
                    final_usage = chunk.usage;
                    println!("Stream finished after {} chunks", chunk_count);
                    break;
                }
            }
            Err(e) => {
                panic!("Stream error: {}", e);
            }
        }
    }

    println!("Accumulated content: {}", accumulated_content);
    assert!(!accumulated_content.is_empty(), "Should receive content");
    assert!(chunk_count > 0, "Should receive at least one chunk");
    assert!(final_usage.is_some(), "Should track token usage");
}

#[tokio::test]
async fn test_openrouter_with_provider_preferences() {
    let Some(api_key) = get_api_key_or_skip() else {
        println!("Skipping test: OPENROUTER_API_KEY not set");
        return;
    };

    let preferences = ProviderPreferences {
        order: Some(vec!["Anthropic".to_string()]),
        allow_fallbacks: Some(true),
        require_parameters: Some(false),
    };

    let provider = OpenRouterProvider::new(
        api_key,
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(100),
        Some(0.7),
    )
    .expect("Failed to create OpenRouter provider")
    .with_provider_preferences(preferences);

    let request = CompletionRequest {
        messages: vec![Message::new(
            MessageRole::User,
            "Reply with 'ok'.".to_string(),
        )],
        max_tokens: Some(50),
        temperature: Some(0.7),
        stream: false,
        tools: None,
        disable_thinking: false,
    };

    let response = provider
        .complete(request)
        .await
        .expect("Completion request with provider preferences failed");

    assert!(!response.content.is_empty());
}

#[tokio::test]
async fn test_openrouter_with_http_headers() {
    let Some(api_key) = get_api_key_or_skip() else {
        println!("Skipping test: OPENROUTER_API_KEY not set");
        return;
    };

    let provider = OpenRouterProvider::new(
        api_key,
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(100),
        Some(0.7),
    )
    .expect("Failed to create OpenRouter provider")
    .with_http_referer("https://example.com".to_string())
    .with_x_title("G3 Test Suite".to_string());

    let request = CompletionRequest {
        messages: vec![Message::new(
            MessageRole::User,
            "Reply with 'ok'.".to_string(),
        )],
        max_tokens: Some(50),
        temperature: Some(0.7),
        stream: false,
        tools: None,
        disable_thinking: false,
    };

    let response = provider
        .complete(request)
        .await
        .expect("Completion request with HTTP headers failed");

    assert!(!response.content.is_empty());
}

#[tokio::test]
async fn test_openrouter_tool_calling() {
    let Some(api_key) = get_api_key_or_skip() else {
        println!("Skipping test: OPENROUTER_API_KEY not set");
        return;
    };

    let provider = OpenRouterProvider::new(
        api_key,
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(500),
        Some(0.7),
    )
    .expect("Failed to create OpenRouter provider");

    let weather_tool = Tool {
        name: "get_weather".to_string(),
        description: "Get the current weather for a location".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city and state, e.g. San Francisco, CA"
                }
            },
            "required": ["location"]
        }),
    };

    let request = CompletionRequest {
        messages: vec![Message::new(
            MessageRole::User,
            "What's the weather like in Tokyo?".to_string(),
        )],
        max_tokens: Some(500),
        temperature: Some(0.7),
        stream: false,
        tools: Some(vec![weather_tool]),
        disable_thinking: false,
    };

    let response = provider
        .complete(request)
        .await
        .expect("Tool calling request failed");

    println!("Response content: {}", response.content);
    println!("Token usage: {:?}", response.usage);

    // Note: Tool calling may or may not be invoked depending on model behavior
    assert!(
        response.usage.total_tokens > 0,
        "Token usage should be tracked"
    );
}

#[test]
fn test_provider_preferences_serialization() {
    let preferences = ProviderPreferences {
        order: Some(vec!["Anthropic".to_string(), "OpenAI".to_string()]),
        allow_fallbacks: Some(true),
        require_parameters: Some(false),
    };

    let json = serde_json::to_value(&preferences).unwrap();
    println!("Provider preferences JSON: {}", json);

    assert!(json.get("order").is_some());
    assert_eq!(json.get("allow_fallbacks").unwrap(), &json!(true));
    assert_eq!(json.get("require_parameters").unwrap(), &json!(false));
}

#[test]
fn test_provider_preferences_partial_serialization() {
    // Test that None fields are omitted from JSON
    let preferences = ProviderPreferences {
        order: None,
        allow_fallbacks: Some(true),
        require_parameters: None,
    };

    let json = serde_json::to_value(&preferences).unwrap();
    println!("Partial provider preferences JSON: {}", json);

    assert!(
        !json.as_object().unwrap().contains_key("order"),
        "None fields should be omitted"
    );
    assert!(json.get("allow_fallbacks").is_some());
    assert!(
        !json.as_object().unwrap().contains_key("require_parameters"),
        "None fields should be omitted"
    );
}

#[test]
fn test_openrouter_provider_trait_implementation() {
    let provider = OpenRouterProvider::new(
        "test_key".to_string(),
        Some("anthropic/claude-3.5-sonnet".to_string()),
        Some(4096),
        Some(0.7),
    )
    .expect("Failed to create provider");

    // Test LLMProvider trait methods
    assert_eq!(provider.name(), "openrouter");
    assert_eq!(provider.model(), "anthropic/claude-3.5-sonnet");
    assert!(provider.has_native_tool_calling());
    assert_eq!(provider.max_tokens(), 4096);
    assert_eq!(provider.temperature(), 0.7);
}

#[test]
fn test_openrouter_provider_with_custom_name() {
    let provider = OpenRouterProvider::new_with_name(
        "openrouter.custom".to_string(),
        "test_key".to_string(),
        Some("openai/gpt-4o".to_string()),
        None,
        None,
    )
    .expect("Failed to create provider");

    assert_eq!(provider.name(), "openrouter.custom");
    assert_eq!(provider.model(), "openai/gpt-4o");
}

#[test]
fn test_openrouter_default_model() {
    let provider = OpenRouterProvider::new(
        "test_key".to_string(),
        None, // No model specified
        None,
        None,
    )
    .expect("Failed to create provider");

    assert_eq!(provider.model(), "anthropic/claude-3.5-sonnet");
}
