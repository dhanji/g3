mod streaming;
pub mod mock;
pub use mock::{MockProvider, MockResponse, MockChunk};

pub use streaming::{decode_utf8_streaming, is_incomplete_json_error, make_final_chunk, make_text_chunk, make_tool_chunk};

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Suffix appended to the default provider's name to form the registry key of
/// its overload fallback (e.g. `anthropic.default` -> `anthropic.default#fallback`).
///
/// `#` is chosen because provider references are parsed by splitting on `.`,
/// so a suffix containing a dot would be read as a *config name* and every
/// config lookup (max_tokens, cache, 1M-context beta) would miss and silently
/// fall back to defaults. `#` cannot appear in a TOML bare key, so it also
/// cannot collide with a real user-defined provider name.
///
/// `g3_core::provider_config::parse_provider_ref` strips this suffix, which is
/// what makes the fallback inherit the default provider's configuration.
pub const FALLBACK_PROVIDER_SUFFIX: &str = "#fallback";

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

    /// Get the context window size for this provider
    /// Returns None if the provider doesn't have a fixed context window
    fn context_window_size(&self) -> Option<u32> {
        None
    }
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

/// A tool call stored in an assistant message for proper API roundtripping.
/// When the model makes a native tool call, we store it structurally so that
/// convert_messages() can send it as a proper tool_use block (not inline JSON text).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    #[serde(skip)]
    pub images: Vec<ImageContent>,
    /// Stable per-message identity, PERSISTED.
    ///
    /// This used to be `#[serde(skip)]`, which meant identity existed only in
    /// memory and was thrown away at the persistence boundary — every reload
    /// produced `""`. Consumers that needed to say "resume after this message"
    /// were forced to use an ARRAY INDEX instead, which is not stable: context
    /// compaction rewrites `conversation_history` in place and shorter, so a
    /// previously-valid index silently points somewhere else (or past the end).
    ///
    /// Persisting it makes "everything after message X" expressible in a way
    /// that survives compaction. `default` so sessions written before this
    /// change still load; `hydrate_message_ids()` backfills them.
    #[serde(default)]
    pub id: String,
    #[serde(skip)]
    pub kind: MessageKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    /// Structured tool calls made by the assistant in this message.
    /// When non-empty, convert_messages() should emit tool_use content blocks
    /// instead of (or in addition to) plain text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<MessageToolCall>,
    /// If this is a tool result message, the ID of the tool_use it responds to.
    /// When set, convert_messages() should emit a tool_result content block
    /// instead of plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// Special message kinds for context management (ACD)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MessageKind {
    /// Regular conversation message
    #[default]
    Regular,
    /// Dehydrated context stub (contains fragment reference)
    DehydratedStub,
    /// Summary of dehydrated context (the response that followed dehydration)
    Summary,
    /// Rehydrated content (restored from a fragment)
    Rehydrated,
}

/// Image content for multimodal messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContent {
    /// Media type (e.g., "image/png", "image/jpeg", "image/gif", "image/webp")
    pub media_type: String,
    /// Base64-encoded image data
    pub data: String,
}

impl ImageContent {
    pub fn new(media_type: &str, data: String) -> Self {
        Self {
            media_type: media_type.to_string(),
            data,
        }
    }

    /// Detect media type from file extension
    pub fn media_type_from_extension(ext: &str) -> Option<&'static str> {
        match ext.to_lowercase().as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            _ => None,
        }
    }

    /// Detect media type from image data magic bytes (file signature)
    /// This is more reliable than file extension as it checks actual content
    pub fn media_type_from_bytes(bytes: &[u8]) -> Option<&'static str> {
        if bytes.len() < 12 {
            return None;
        }

        // PNG: 89 50 4E 47 0D 0A 1A 0A
        if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
            return Some("image/png");
        }

        // JPEG: FF D8 FF
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some("image/jpeg");
        }

        // GIF: 47 49 46 38 (GIF8)
        if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
            return Some("image/gif");
        }

        // WebP: 52 49 46 46 ... 57 45 42 50 (RIFF....WEBP)
        if bytes.starts_with(&[0x52, 0x49, 0x46, 0x46]) && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
            return Some("image/webp");
        }

        None
    }
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
    /// Tokens written to cache (Anthropic: cache_creation_input_tokens)
    #[serde(default)]
    pub cache_creation_tokens: u32,
    /// Tokens read from cache (Anthropic: cache_read_input_tokens, OpenAI: cached_tokens)
    #[serde(default)]
    pub cache_read_tokens: u32,
}

pub type CompletionStream = tokio_stream::wrappers::ReceiverStream<Result<CompletionChunk>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionChunk {
    pub content: String,
    pub finished: bool,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<Usage>, // Add usage tracking for streaming
    /// Stop reason from the API (e.g., "end_turn", "max_tokens", "stop_sequence")
    pub stop_reason: Option<String>,
    /// Tool call currently being streamed (name only, for UI hint)
    pub tool_call_streaming: Option<String>,
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
pub mod gemini;
pub mod oauth;
pub mod openai;

pub use anthropic::AnthropicProvider;
pub use databricks::DatabricksProvider;
pub use embedded::EmbeddedProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAIProvider;

impl Message {
    /// Generate a unique message ID in format HHMMSS-XXXXXX
    /// where the suffix is 6 random alphanumeric characters.
    ///
    /// Now that this id is PERSISTED and used as a resume cursor, uniqueness is
    /// load-bearing rather than cosmetic. The old format used a 3-char suffix
    /// (~140k combinations) inside a 1-second bucket, and a tool loop can append
    /// many messages within the same second — a collision there would make
    /// "resume after message X" ambiguous, silently replaying or skipping a
    /// span. 6 chars is ~19 billion per second, which retires the concern.
    fn generate_id() -> String {
        let now = chrono::Local::now();
        let timestamp = now.format("%H%M%S").to_string();

        let mut rng = rand::thread_rng();
        let random_chars: String = (0..6)
            .map(|_| {
                let chars = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
                let idx = rng.gen_range(0..chars.len());
                chars[idx] as char
            })
            .collect();

        format!("{}-{}", timestamp, random_chars)
    }

    /// Public wrapper so other crates can mint ids for messages that predate
    /// id persistence (see `ContextWindow::hydrate_message_ids`).
    pub fn generate_id_public() -> String {
        Self::generate_id()
    }

    /// Create a new message with optional cache control
    pub fn new(role: MessageRole, content: String) -> Self {
        Self {
            role,
            content,
            images: Vec::new(),
            id: Self::generate_id(),
            kind: MessageKind::Regular,
            cache_control: None,
            tool_calls: Vec::new(),
            tool_result_id: None,
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
            images: Vec::new(),
            id: Self::generate_id(),
            kind: MessageKind::Regular,
            cache_control: Some(cache_control),
            tool_calls: Vec::new(),
            tool_result_id: None,
        }
    }

    /// Create a new message with a specific kind (for ACD)
    pub fn with_kind(role: MessageRole, content: String, kind: MessageKind) -> Self {
        Self {
            role,
            content,
            images: Vec::new(),
            id: Self::generate_id(),
            kind,
            cache_control: None,
            tool_calls: Vec::new(),
            tool_result_id: None,
        }
    }

    /// Check if this message is a dehydrated stub
    pub fn is_dehydrated_stub(&self) -> bool {
        self.kind == MessageKind::DehydratedStub
    }

    /// Check if this message is a summary
    pub fn is_summary(&self) -> bool {
        self.kind == MessageKind::Summary
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
    /// Registry key of the overload fallback provider, if one was registered
    /// (`--fallback-model`). `None` means the feature is off and this type
    /// behaves exactly as it did before it existed.
    fallback_provider: Option<String>,
    /// Whether the fallback is currently standing in for the default.
    ///
    /// An `AtomicBool` rather than a plain `bool` because activation happens
    /// deep inside the streaming retry loop, which holds only `&self` — the
    /// alternative was threading `&mut` through a dozen call sites (`get(None)`
    /// is called from 17 places) purely to flip one flag. `Relaxed` is
    /// sufficient: the flag is set and read from the same task, and no other
    /// memory ordering depends on it.
    fallback_active: std::sync::atomic::AtomicBool,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            default_provider: String::new(),
            fallback_provider: None,
            fallback_active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn register<P: LLMProvider + 'static>(&mut self, provider: P) {
        let name = provider.name().to_string();
        self.providers.insert(name.clone(), Box::new(provider));

        if self.default_provider.is_empty() {
            self.default_provider = name;
        }
    }

    /// Register `provider` as the overload fallback for the default provider.
    ///
    /// The provider is stored in the same map as everything else, so it remains
    /// addressable by name; what makes it special is that [`Self::get`] will
    /// return it in place of the default while the fallback is active.
    ///
    /// Note this does NOT set `default_provider` even if the registry was empty
    /// — a fallback must never become the default, or an overload would become
    /// permanent instead of lasting one turn.
    pub fn register_fallback<P: LLMProvider + 'static>(&mut self, provider: P) {
        let name = provider.name().to_string();
        // NOTE: deliberately inserts into the map directly instead of calling
        // `register()`. `register()` implements "the first provider registered
        // becomes the default", so routing the fallback through it would make
        // the fallback the permanent default whenever it happened to be
        // registered first — converting a one-turn degradation into a
        // forever one. Covered by
        // `test_fallback_registered_first_does_not_become_default`.
        self.providers.insert(name.clone(), Box::new(provider));
        self.fallback_provider = Some(name);
    }

    /// Whether a fallback provider is available at all.
    pub fn has_fallback(&self) -> bool {
        self.fallback_provider.is_some()
    }

    /// Registry key of the fallback provider, if registered.
    pub fn fallback_name(&self) -> Option<&str> {
        self.fallback_provider.as_deref()
    }

    /// Model string of the fallback provider, if registered.
    pub fn fallback_model(&self) -> Option<&str> {
        let name = self.fallback_provider.as_deref()?;
        self.providers.get(name).map(|p| p.model())
    }

    /// Route `get(None)` to the fallback provider.
    ///
    /// Returns `false` (and changes nothing) when no fallback is registered, so
    /// callers can use the return value to decide whether to tell the user
    /// anything. Idempotent.
    pub fn activate_fallback(&self) -> bool {
        if self.fallback_provider.is_none() {
            return false;
        }
        self.fallback_active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Route `get(None)` back to the default provider. Idempotent, and safe to
    /// call unconditionally — which is exactly how the per-turn reset uses it.
    pub fn deactivate_fallback(&self) {
        self.fallback_active
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the fallback is currently standing in for the default.
    pub fn is_fallback_active(&self) -> bool {
        self.fallback_active
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_default(&mut self, provider_name: &str) -> Result<()> {
        if !self.providers.contains_key(provider_name) {
            anyhow::bail!("Provider '{}' not found", provider_name);
        }
        self.default_provider = provider_name.to_string();
        Ok(())
    }

    /// Resolve a provider.
    ///
    /// `Some(name)` is an explicit request and is ALWAYS honoured verbatim —
    /// fallback state is ignored — so a caller that deliberately addressed one
    /// model cannot be silently handed another.
    ///
    /// `None` means "the current provider", which is the fallback while it is
    /// active and the default otherwise.
    pub fn get(&self, provider_name: Option<&str>) -> Result<&dyn LLMProvider> {
        let name = match provider_name {
            Some(explicit) => explicit,
            None => self.current_provider_name(),
        };
        self.providers
            .get(name)
            .map(|p| p.as_ref())
            .ok_or_else(|| anyhow::anyhow!("Provider '{}' not found", name))
    }

    /// Name of the provider `get(None)` currently resolves to.
    pub fn current_provider_name(&self) -> &str {
        if self.is_fallback_active() {
            if let Some(fallback) = self.fallback_provider.as_deref() {
                return fallback;
            }
        }
        &self.default_provider
    }

    /// Name of the configured default provider, regardless of fallback state.
    pub fn default_provider_name(&self) -> &str {
        &self.default_provider
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
        assert_eq!(parts.len(), 2, "Message ID should have format HHMMSS-XXXXXX");

        // Check timestamp part is 6 digits
        assert_eq!(parts[0].len(), 6, "Timestamp should be 6 digits (HHMMSS)");
        assert!(
            parts[0].chars().all(|c| c.is_ascii_digit()),
            "Timestamp should be all digits"
        );

        // Check random part is 6 alpha characters. Widened from 3 when the id
        // became a persisted resume cursor — see generate_id().
        assert_eq!(parts[1].len(), 6, "Random part should be 6 characters");
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
    fn test_message_id_is_persisted_and_round_trips() {
        // Inverted deliberately (2026-08-14). This test used to assert the id
        // was NOT serialized. That contract is what forced consumers to use
        // array indices to say "resume after this message", which breaks the
        // moment context compaction rewrites history shorter. The id is now the
        // cursor, so persistence is the contract.
        let msg = Message::new(MessageRole::User, "Hello".to_string());
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"id\""), "id must be serialized: {}", json);

        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id, "id must survive a round trip unchanged");
        assert!(!back.id.is_empty());
    }

    #[test]
    fn test_message_without_id_field_deserializes_to_empty() {
        // Sessions written before ids were persisted have no `id` key at all.
        // They must LOAD (not error), yielding an empty id for
        // ContextWindow::hydrate_message_ids to backfill.
        let json = r#"{"role":"user","content":"legacy"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id, "", "missing id should default to empty, not fail");
        assert_eq!(msg.content, "legacy");
    }

    #[test]
    fn test_generated_ids_are_unique_within_the_same_second() {
        // The id is a resume cursor now, so a collision would make "everything
        // after X" ambiguous. A tool loop can append many messages inside one
        // timestamp bucket, so uniqueness must not rely on the clock.
        use std::collections::HashSet;
        let ids: HashSet<String> = (0..2000).map(|_| Message::generate_id()).collect();
        assert_eq!(ids.len(), 2000, "generated ids collided within one second");
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

#[cfg(test)]
mod fallback_registry_tests {
    use super::*;
    use crate::mock::MockProvider;

    /// Registry holding a default provider and (optionally) a fallback.
    fn registry_with_fallback() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry.register(
            MockProvider::new()
                .with_name("anthropic.default")
                .with_model("claude-opus-5"),
        );
        registry.register_fallback(
            MockProvider::new()
                .with_name("anthropic.default#fallback")
                .with_model("claude-opus-4-8"),
        );
        registry
    }

    fn registry_without_fallback() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry.register(
            MockProvider::new()
                .with_name("anthropic.default")
                .with_model("claude-opus-5"),
        );
        registry
    }

    #[test]
    fn test_activate_routes_get_none_to_fallback() {
        let registry = registry_with_fallback();

        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
        assert!(registry.activate_fallback());
        assert!(registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-4-8");
        assert_eq!(
            registry.current_provider_name(),
            "anthropic.default#fallback"
        );
    }

    #[test]
    fn test_deactivate_returns_to_default() {
        let registry = registry_with_fallback();
        registry.activate_fallback();
        registry.deactivate_fallback();

        assert!(!registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
        assert_eq!(registry.current_provider_name(), "anthropic.default");
    }

    /// Negative: with the feature off, activation is a no-op rather than an
    /// error or a panic, and resolution is unchanged.
    #[test]
    fn test_activate_without_fallback_is_a_noop() {
        let registry = registry_without_fallback();

        assert!(!registry.has_fallback());
        assert!(
            !registry.activate_fallback(),
            "activate must report false so callers do not announce a switch that did not happen"
        );
        assert!(!registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
    }

    /// Negative: even if the flag were somehow set with no fallback registered,
    /// resolution must not break.
    #[test]
    fn test_active_flag_without_fallback_still_resolves_default() {
        let registry = registry_without_fallback();
        registry
            .fallback_active
            .store(true, std::sync::atomic::Ordering::Relaxed);

        assert_eq!(registry.current_provider_name(), "anthropic.default");
        assert!(registry.get(None).is_ok());
    }

    /// An explicit request is never redirected — otherwise a caller that
    /// deliberately named a model could silently get a different one.
    #[test]
    fn test_explicit_get_is_unaffected_by_fallback_state() {
        let registry = registry_with_fallback();
        registry.activate_fallback();

        assert_eq!(
            registry.get(Some("anthropic.default")).unwrap().model(),
            "claude-opus-5"
        );
        assert_eq!(
            registry
                .get(Some("anthropic.default#fallback"))
                .unwrap()
                .model(),
            "claude-opus-4-8"
        );
    }

    #[test]
    fn test_activate_and_deactivate_are_idempotent() {
        let registry = registry_with_fallback();

        assert!(registry.activate_fallback());
        assert!(registry.activate_fallback());
        assert!(registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-4-8");

        registry.deactivate_fallback();
        registry.deactivate_fallback();
        assert!(!registry.is_fallback_active());
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
    }

    /// BOUNDARY / the nastiest case: if the fallback is registered FIRST (empty
    /// registry), the "first registration becomes the default" rule would make
    /// the fallback the permanent default — turning a one-turn degradation into
    /// a forever one. register_fallback must refuse to claim the default slot.
    #[test]
    fn test_fallback_registered_first_does_not_become_default() {
        let mut registry = ProviderRegistry::new();
        registry.register_fallback(
            MockProvider::new()
                .with_name("anthropic.default#fallback")
                .with_model("claude-opus-4-8"),
        );
        registry.register(
            MockProvider::new()
                .with_name("anthropic.default")
                .with_model("claude-opus-5"),
        );

        assert_eq!(
            registry.default_provider_name(),
            "anthropic.default",
            "the fallback must never be installed as the default provider"
        );
        assert_eq!(registry.get(None).unwrap().model(), "claude-opus-5");
    }

    #[test]
    fn test_fallback_name_and_model_accessors() {
        let registry = registry_with_fallback();
        assert_eq!(registry.fallback_name(), Some("anthropic.default#fallback"));
        assert_eq!(registry.fallback_model(), Some("claude-opus-4-8"));

        let bare = registry_without_fallback();
        assert_eq!(bare.fallback_name(), None);
        assert_eq!(bare.fallback_model(), None);
    }

    /// The default provider itself is still reported correctly while the
    /// fallback is active — needed so the retry path can name both models.
    #[test]
    fn test_default_provider_name_survives_activation() {
        let registry = registry_with_fallback();
        registry.activate_fallback();
        assert_eq!(registry.default_provider_name(), "anthropic.default");
        assert_eq!(
            registry.current_provider_name(),
            "anthropic.default#fallback"
        );
    }
}
