//! Retry wrapper for LLM providers
//!
//! This module provides a wrapper that adds retry functionality to any LLM provider.

use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::{
    CompletionRequest, CompletionResponse, CompletionStream, LLMProvider,
    retry::{execute_with_retry, RetryConfig},
};

/// A wrapper that adds retry functionality to any LLM provider
pub struct RetryProvider<P: LLMProvider> {
    inner: P,
    config: RetryConfig,
}

impl<P: LLMProvider> RetryProvider<P> {
    /// Create a new retry provider with the default retry configuration
    pub fn new(provider: P) -> Self {
        Self {
            inner: provider,
            config: RetryConfig::default(),
        }
    }

    /// Create a new retry provider with a custom retry configuration
    pub fn with_config(provider: P, config: RetryConfig) -> Self {
        Self {
            inner: provider,
            config,
        }
    }

    /// Get a reference to the inner provider
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Get a mutable reference to the inner provider
    pub fn inner_mut(&mut self) -> &mut P {
        &mut self.inner
    }

    /// Get the retry configuration
    pub fn config(&self) -> &RetryConfig {
        &self.config
    }

    /// Set the retry configuration
    pub fn set_config(&mut self, config: RetryConfig) {
        self.config = config;
    }
}

#[async_trait]
impl<P: LLMProvider> LLMProvider for RetryProvider<P> {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        debug!(
            "Processing completion request with retry for provider: {}",
            self.inner.name()
        );

        execute_with_retry(
            &format!("{} completion request", self.inner.name()),
            || {
                let req = request.clone();
                let provider = &self.inner;
                async move { provider.complete(req).await }
            },
            &self.config,
        )
        .await
    }

    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream> {
        debug!(
            "Processing streaming request with retry for provider: {}",
            self.inner.name()
        );

        // Note: Streaming retry is more complex. We retry the initial connection,
        // but once streaming starts, we don't retry mid-stream.
        execute_with_retry(
            &format!("{} streaming request", self.inner.name()),
            || {
                let req = request.clone();
                let provider = &self.inner;
                async move { provider.stream(req).await }
            },
            &self.config,
        )
        .await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn has_native_tool_calling(&self) -> bool {
        self.inner.has_native_tool_calling()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, MessageRole, Usage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    // Mock provider for testing
    struct MockProvider {
        name: String,
        model: String,
        fail_count: Arc<AtomicUsize>,
        max_failures: usize,
    }

    impl MockProvider {
        fn new(max_failures: usize) -> Self {
            Self {
                name: "mock".to_string(),
                model: "mock-model".to_string(),
                fail_count: Arc::new(AtomicUsize::new(0)),
                max_failures,
            }
        }
    }

    #[async_trait]
    impl LLMProvider for MockProvider {
        async fn complete(&self, _request: CompletionRequest) -> Result<CompletionResponse> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.max_failures {
                Err(anyhow::anyhow!("Connection reset"))
            } else {
                Ok(CompletionResponse {
                    content: "Success".to_string(),
                    usage: Usage {
                        prompt_tokens: 10,
                        completion_tokens: 5,
                        total_tokens: 15,
                    },
                    model: self.model.clone(),
                })
            }
        }

        async fn stream(&self, _request: CompletionRequest) -> Result<CompletionStream> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.max_failures {
                Err(anyhow::anyhow!("Connection timeout"))
            } else {
                let (tx, rx) = mpsc::channel(1);
                // Send a dummy chunk
                tokio::spawn(async move {
                    let _ = tx.send(Ok(crate::CompletionChunk {
                        content: "Streamed".to_string(),
                        finished: true,
                        tool_calls: None,
                    })).await;
                });
                Ok(ReceiverStream::new(rx))
            }
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn model(&self) -> &str {
            &self.model
        }
    }

    #[tokio::test]
    async fn test_retry_provider_complete_success_after_retries() {
        let mock = MockProvider::new(2); // Fail twice, then succeed
        let retry_provider = RetryProvider::new(mock);

        let request = CompletionRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: "Test".to_string(),
            }],
            max_tokens: None,
            temperature: None,
            stream: false,
            tools: None,
        };

        let result = retry_provider.complete(request).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "Success");
    }

    #[tokio::test]
    async fn test_retry_provider_complete_failure_after_max_retries() {
        let mock = MockProvider::new(10); // Always fail
        let mut retry_provider = RetryProvider::new(mock);
        retry_provider.set_config(RetryConfig::new(2, 10, 100, 2.0)); // Fast retries for testing

        let request = CompletionRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: "Test".to_string(),
            }],
            max_tokens: None,
            temperature: None,
            stream: false,
            tools: None,
        };

        let result = retry_provider.complete(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_retry_provider_stream_success_after_retries() {
        let mock = MockProvider::new(1); // Fail once, then succeed
        let retry_provider = RetryProvider::new(mock);

        let request = CompletionRequest {
            messages: vec![Message {
                role: MessageRole::User,
                content: "Test".to_string(),
            }],
            max_tokens: None,
            temperature: None,
            stream: true,
            tools: None,
        };

        let result = retry_provider.stream(request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_retry_provider_preserves_inner_properties() {
        let mock = MockProvider::new(0);
        let retry_provider = RetryProvider::new(mock);

        assert_eq!(retry_provider.name(), "mock");
        assert_eq!(retry_provider.model(), "mock-model");
        assert!(!retry_provider.has_native_tool_calling());
    }
}