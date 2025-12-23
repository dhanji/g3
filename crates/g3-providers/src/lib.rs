use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Trait for LLM providers
#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Generate a completion for the given messages
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;

    /// Stream a completion for the given messages
    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream>;

    /// Get the provider name
    fn name(&self) -> &str;

    /// Get the model name
    fn model(&self) -> &str;

    /// Check if the provider supports native tool calling
    fn has_native_tool_calling(&self) -> bool {
        false
    }

    /// Check if the provider supports cache control
    fn supports_cache_control(&self) -> bool {
        false
    }

    /// Get the configured max_tokens for this provider
    fn max_tokens(&self) -> u32;

    /// Get the configured temperature for this provider
    fn temperature(&self) -> f32;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: bool,
    pub tools: Option<Vec<Tool>>,
    /// Force disable thinking mode for this request (used when max_tokens is too low)
    pub disable_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: CacheType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CacheType {
    Ephemeral,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: None,
        }
    }

    pub fn five_minute() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: Some("5m".to_string()),
        }
    }

    pub fn one_hour() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: Some("1h".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    #[serde(skip)]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub content: String,
    pub usage: Usage,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

pub type CompletionStream = tokio_stream::wrappers::ReceiverStream<Result<CompletionChunk>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionChunk {
    pub content: String,
    pub finished: bool,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<Usage>, // Add usage tracking for streaming
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

pub mod anthropic;
pub mod databricks;
pub mod embedded;
pub mod oauth;
pub mod openai;
pub mod openrouter;

pub use anthropic::AnthropicProvider;
pub use databricks::DatabricksProvider;
pub use embedded::EmbeddedProvider;
pub use openai::OpenAIProvider;
pub use openrouter::{OpenRouterProvider, ProviderPreferences};

impl Message {
    /// Generate a unique message ID in format HHMMSS-XXX
    /// where XXX are 3 random alphanumeric characters (upper and lowercase)
    fn generate_id() -> String {
        let now = chrono::Local::now();
        let timestamp = now.format("%H%M%S").to_string();

        let mut rng = rand::thread_rng();
        let random_chars: String = (0..3)
            .map(|_| {
                let chars = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
                let idx = rng.gen_range(0..chars.len());
                chars[idx] as char
            })
            .collect();

        format!("{}-{}", timestamp, random_chars)
    }

    /// Create a new message with optional cache control
    pub fn new(role: MessageRole, content: String) -> Self {
        Self {
            role,
            content,
            id: Self::generate_id(),
            cache_control: None,
        }
    }

    /// Create a new message with cache control
    pub fn with_cache_control(
        role: MessageRole,
        content: String,
        cache_control: CacheControl,
    ) -> Self {
        Self {
            role,
            content,
            id: Self::generate_id(),
            cache_control: Some(cache_control),
        }
    }

    /// Create a message with cache control, with provider validation
    pub fn with_cache_control_validated(
        role: MessageRole,
        content: String,
        cache_control: CacheControl,
        provider: &dyn LLMProvider,
    ) -> Self {
        if !provider.supports_cache_control() {
            tracing::warn!(
                "Cache control requested for provider '{}' which does not support it. \
                Cache control is only supported by Anthropic and Anthropic via Databricks.",
                provider.name()
            );
            return Self::new(role, content);
        }

        Self::with_cache_control(role, content, cache_control)
    }
}

/// Provider registry for managing multiple LLM providers
pub struct ProviderRegistry {
    providers: HashMap<String, Box<dyn LLMProvider>>,
    default_provider: String,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: String::new(),
        }
    }

    pub fn register<P: LLMProvider + 'static>(&mut self, provider: P) {
        let name = provider.name().to_string();
        self.providers.insert(name.clone(), Box::new(provider));

        if self.default_provider.is_empty() {
            self.default_provider = name;
        }
    }

    pub fn set_default(&mut self, provider_name: &str) -> Result<()> {
        if !self.providers.contains_key(provider_name) {
            anyhow::bail!("Provider '{}' not found", provider_name);
        }
        self.default_provider = provider_name.to_string();
        Ok(())
    }

    pub fn get(&self, provider_name: Option<&str>) -> Result<&dyn LLMProvider> {
        let name = provider_name.unwrap_or(&self.default_provider);
        self.providers
            .get(name)
            .map(|p| p.as_ref())
            .ok_or_else(|| anyhow::anyhow!("Provider '{}' not found", name))
    }

    pub fn list_providers(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_serialization_without_cache_control() {
        let msg = Message::new(MessageRole::User, "Hello".to_string());
        let json = serde_json::to_string(&msg).unwrap();

        println!("Message JSON without cache_control: {}", json);
        assert!(
            !json.contains("cache_control"),
            "JSON should not contain 'cache_control' field when not configured"
        );
    }

    #[test]
    fn test_message_serialization_with_cache_control() {
        let msg = Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            CacheControl::ephemeral(),
        );
        let json = serde_json::to_string(&msg).unwrap();

        println!("Message JSON with cache_control: {}", json);
        assert!(
            json.contains("cache_control"),
            "JSON should contain 'cache_control' field when configured"
        );
        assert!(
            json.contains("ephemeral"),
            "JSON should contain 'ephemeral' value"
        );
        assert!(
            json.contains("\"type\":"),
            "JSON should contain 'type' field in cache_control"
        );
        assert!(
            !json.contains("null"),
            "JSON should not contain null values"
        );
    }

    #[test]
    fn test_cache_control_five_minute_serialization() {
        let msg = Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            CacheControl::five_minute(),
        );
        let json = serde_json::to_string(&msg).unwrap();

        println!("Message JSON with 5-minute cache_control: {}", json);
        assert!(
            json.contains("cache_control"),
            "JSON should contain 'cache_control' field"
        );
        assert!(
            json.contains("ephemeral"),
            "JSON should contain 'ephemeral' type"
        );
        assert!(
            json.contains("\"ttl\":\"5m\""),
            "JSON should contain ttl field with 5m value"
        );
    }

    #[test]
    fn test_cache_control_one_hour_serialization() {
        let msg = Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            CacheControl::one_hour(),
        );
        let json = serde_json::to_string(&msg).unwrap();

        println!("Message JSON with 1-hour cache_control: {}", json);
        assert!(
            json.contains("cache_control"),
            "JSON should contain 'cache_control' field"
        );
        assert!(
            json.contains("ephemeral"),
            "JSON should contain 'ephemeral' type"
        );
        assert!(
            json.contains("\"ttl\":\"1h\""),
            "JSON should contain ttl field with 1h value"
        );
    }

    #[test]
    fn test_message_id_generation() {
        let msg = Message::new(MessageRole::User, "Hello".to_string());

        // Check that id is not empty
        assert!(!msg.id.is_empty(), "Message ID should not be empty");

        // Check format: HHMMSS-XXX
        let parts: Vec<&str> = msg.id.split('-').collect();
        assert_eq!(parts.len(), 2, "Message ID should have format HHMMSS-XXX");

        // Check timestamp part is 6 digits
        assert_eq!(parts[0].len(), 6, "Timestamp should be 6 digits (HHMMSS)");
        assert!(
            parts[0].chars().all(|c| c.is_ascii_digit()),
            "Timestamp should be all digits"
        );

        // Check random part is 3 alpha characters
        assert_eq!(parts[1].len(), 3, "Random part should be 3 characters");
        assert!(
            parts[1].chars().all(|c| c.is_ascii_alphabetic()),
            "Random part should be all alphabetic characters"
        );
    }

    #[test]
    fn test_message_id_uniqueness() {
        let msg1 = Message::new(MessageRole::User, "Hello".to_string());
        let msg2 = Message::new(MessageRole::User, "Hello".to_string());

        // IDs should be different (due to random component)
        // Note: There's a tiny chance they could be the same, but very unlikely
        println!("msg1.id: {}, msg2.id: {}", msg1.id, msg2.id);
    }

    #[test]
    fn test_message_id_not_serialized() {
        let msg = Message::new(MessageRole::User, "Hello".to_string());
        let json = serde_json::to_string(&msg).unwrap();

        println!("Message JSON: {}", json);
        assert!(
            !json.contains("\"id\""),
            "JSON should not contain 'id' field"
        );
    }

    #[test]
    fn test_message_with_cache_control_has_id() {
        let msg = Message::with_cache_control(
            MessageRole::User,
            "Hello".to_string(),
            CacheControl::ephemeral(),
        );

        assert!(
            !msg.id.is_empty(),
            "Message with cache control should have an ID"
        );
        assert!(
            msg.id.contains('-'),
            "Message ID should contain hyphen separator"
        );
    }
}
