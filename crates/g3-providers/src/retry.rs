//! Retry logic for LLM providers
//!
//! This module provides a generic retry mechanism with exponential backoff
//! that can be used by all LLM providers to handle transient failures.

use anyhow::{anyhow, Result};
use std::future::Future;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Configuration for retry behavior
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_retries: u32,
    /// Initial delay between retries in milliseconds
    pub initial_delay_ms: u64,
    /// Maximum delay between retries in milliseconds
    pub max_delay_ms: u64,
    /// Multiplier for exponential backoff
    pub multiplier: f64,
    /// Whether to retry on timeout errors
    pub retry_on_timeout: bool,
    /// Whether to retry on rate limit errors
    pub retry_on_rate_limit: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 10000,
            multiplier: 2.0,
            retry_on_timeout: true,
            retry_on_rate_limit: true,
        }
    }
}

impl RetryConfig {
    /// Create a new retry configuration with custom settings
    pub fn new(
        max_retries: u32,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        multiplier: f64,
    ) -> Self {
        Self {
            max_retries,
            initial_delay_ms,
            max_delay_ms,
            multiplier,
            retry_on_timeout: true,
            retry_on_rate_limit: true,
        }
    }

    /// Create a configuration with no retries
    pub fn no_retry() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Create a configuration for aggressive retry (useful for critical operations)
    pub fn aggressive() -> Self {
        Self {
            max_retries: 5,
            initial_delay_ms: 500,
            max_delay_ms: 30000,
            multiplier: 1.5,
            retry_on_timeout: true,
            retry_on_rate_limit: true,
        }
    }

    /// Create a configuration for gentle retry (useful for non-critical operations)
    pub fn gentle() -> Self {
        Self {
            max_retries: 2,
            initial_delay_ms: 2000,
            max_delay_ms: 5000,
            multiplier: 2.0,
            retry_on_timeout: true,
            retry_on_rate_limit: true,
        }
    }
}

/// Determines if an error is retryable based on common patterns
pub fn is_retryable_error(error: &anyhow::Error, config: &RetryConfig) -> bool {
    let error_str = error.to_string().to_lowercase();
    
    // Always retry on these conditions
    let always_retry = error_str.contains("connection reset") ||
        error_str.contains("connection closed") ||
        error_str.contains("connection refused") ||
        error_str.contains("broken pipe") ||
        error_str.contains("502") ||  // Bad gateway
        error_str.contains("503") ||  // Service unavailable
        error_str.contains("504");    // Gateway timeout

    if always_retry {
        return true;
    }

    // Conditionally retry based on configuration
    if config.retry_on_timeout && (error_str.contains("timed out") || error_str.contains("timeout")) {
        return true;
    }

    if config.retry_on_rate_limit && (error_str.contains("429") || error_str.contains("rate limit")) {
        return true;
    }

    // Check for specific provider errors that should be retried
    if error_str.contains("temporarily unavailable") ||
       error_str.contains("service is busy") ||
       error_str.contains("please try again") {
        return true;
    }

    false
}

/// Execute an async operation with exponential backoff retry
pub async fn execute_with_retry<F, Fut, T>(
    operation_name: &str,
    mut operation: F,
    config: &RetryConfig,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut retry_count = 0;
    let mut delay_ms = config.initial_delay_ms;

    loop {
        match operation().await {
            Ok(result) => {
                if retry_count > 0 {
                    info!("{} succeeded after {} retries", operation_name, retry_count);
                }
                return Ok(result);
            }
            Err(e) => {
                // Check if this is a retryable error
                let is_retryable = is_retryable_error(&e, config);

                if !is_retryable || retry_count >= config.max_retries {
                    if retry_count > 0 {
                        error!("{} failed after {} retries: {}", operation_name, retry_count, e);
                    } else {
                        debug!("{} failed (non-retryable): {}", operation_name, e);
                    }
                    return Err(e);
                }

                warn!(
                    "{} failed (attempt {}/{}): {}. Retrying in {}ms...",
                    operation_name, 
                    retry_count + 1, 
                    config.max_retries, 
                    e, 
                    delay_ms
                );

                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                
                retry_count += 1;
                delay_ms = ((delay_ms as f64 * config.multiplier) as u64).min(config.max_delay_ms);
            }
        }
    }
}

/// Execute an async operation with retry, using a custom error checker
pub async fn execute_with_custom_retry<F, Fut, T, E>(
    operation_name: &str,
    mut operation: F,
    config: &RetryConfig,
    is_retryable: E,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    E: Fn(&anyhow::Error) -> bool,
{
    let mut retry_count = 0;
    let mut delay_ms = config.initial_delay_ms;

    loop {
        match operation().await {
            Ok(result) => {
                if retry_count > 0 {
                    info!("{} succeeded after {} retries", operation_name, retry_count);
                }
                return Ok(result);
            }
            Err(e) => {
                if !is_retryable(&e) || retry_count >= config.max_retries {
                    if retry_count > 0 {
                        error!("{} failed after {} retries: {}", operation_name, retry_count, e);
                    } else {
                        debug!("{} failed (non-retryable): {}", operation_name, e);
                    }
                    return Err(e);
                }

                warn!(
                    "{} failed (attempt {}/{}): {}. Retrying in {}ms...",
                    operation_name, 
                    retry_count + 1, 
                    config.max_retries, 
                    e, 
                    delay_ms
                );

                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                
                retry_count += 1;
                delay_ms = ((delay_ms as f64 * config.multiplier) as u64).min(config.max_delay_ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 10000);
        assert_eq!(config.multiplier, 2.0);
    }

    #[test]
    fn test_retry_config_no_retry() {
        let config = RetryConfig::no_retry();
        assert_eq!(config.max_retries, 0);
    }

    #[test]
    fn test_is_retryable_error() {
        let config = RetryConfig::default();
        
        // Test timeout errors
        let timeout_error = anyhow!("Request timed out");
        assert!(is_retryable_error(&timeout_error, &config));
        
        // Test connection errors
        let connection_error = anyhow!("Connection reset by peer");
        assert!(is_retryable_error(&connection_error, &config));
        
        // Test rate limit errors
        let rate_limit_error = anyhow!("429 Too Many Requests");
        assert!(is_retryable_error(&rate_limit_error, &config));
        
        // Test non-retryable errors
        let auth_error = anyhow!("401 Unauthorized");
        assert!(!is_retryable_error(&auth_error, &config));
        
        let not_found_error = anyhow!("404 Not Found");
        assert!(!is_retryable_error(&not_found_error, &config));
    }

    #[tokio::test]
    async fn test_execute_with_retry_success() {
        let config = RetryConfig::default();
        let attempt = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_clone = attempt.clone();
        
        let result: Result<&str> = execute_with_retry(
            "test_operation",
            || {
                let count = attempt_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if count < 2 {
                    Err(anyhow!("Connection reset"))
                } else {
                    Ok("success")
                }
                }
            },
            &config,
        ).await;
        
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "success");
    }

    #[tokio::test]
    async fn test_execute_with_retry_max_attempts() {
        let config = RetryConfig::new(2, 10, 100, 2.0);
        let attempt = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_clone = attempt.clone();
        
        let result: Result<&str> = execute_with_retry(
            "test_operation",
            || {
                attempt_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Err(anyhow!("Connection reset")) }
            },
            &config,
        ).await;
        
        assert!(result.is_err());
        assert_eq!(attempt.load(std::sync::atomic::Ordering::SeqCst), 3); // Initial attempt + 2 retries
    }

    #[tokio::test]
    async fn test_execute_with_retry_non_retryable() {
        let config = RetryConfig::default();
        let attempt = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempt_clone = attempt.clone();
        
        let result: Result<&str> = execute_with_retry(
            "test_operation",
            || {
                attempt_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Err(anyhow!("401 Unauthorized")) }
            },
            &config,
        ).await;
        
        assert!(result.is_err());
        assert_eq!(attempt.load(std::sync::atomic::Ordering::SeqCst), 1); // Should not retry on non-retryable error
    }
}