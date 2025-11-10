//! Integration tests for error handling with retry logic

#[cfg(test)]
mod tests {
    use crate::error_handling::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_with_recoverable_error() {
        let attempt_count = Arc::new(AtomicU32::new(0));

        let context = ErrorContext::new(
            "test_operation".to_string(),
            "test_provider".to_string(),
            "test_model".to_string(),
            "test prompt".to_string(),
            None,
            100,
            false,  // quiet parameter
        );

        let result = retry_with_backoff(
            "test_operation",
            || {
                let counter = Arc::clone(&attempt_count);
                async move {
                    let count = counter.fetch_add(1, Ordering::SeqCst);
                    if count < 2 {
                        // Fail with recoverable error on first two attempts
                        Err(anyhow::anyhow!("Rate limit exceeded"))
                    } else {
                        // Succeed on third attempt
                        Ok("Success")
                    }
                }
            },
            &context,
            false, // not autonomous mode
            3,     // max_attempts
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Success");
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_non_recoverable_error() {
        let attempt_count = Arc::new(AtomicU32::new(0));

        let context = ErrorContext::new(
            "test_operation".to_string(),
            "test_provider".to_string(),
            "test_model".to_string(),
            "test prompt".to_string(),
            None,
            100,
            false,  // quiet parameter
        );

        let result: Result<&str, _> = retry_with_backoff(
            "test_operation",
            || {
                let counter = Arc::clone(&attempt_count);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Always fail with non-recoverable error
                    Err(anyhow::anyhow!("Invalid API key"))
                }
            },
            &context,
            false, // not autonomous mode
            3,     // max_attempts
        )
        .await;

        assert!(result.is_err());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 1); // Should only try once
    }

    #[tokio::test]
    async fn test_retry_exhaustion() {
        let attempt_count = Arc::new(AtomicU32::new(0));

        let context = ErrorContext::new(
            "test_operation".to_string(),
            "test_provider".to_string(),
            "test_model".to_string(),
            "test prompt".to_string(),
            None,
            100,
            false,  // quiet parameter
        );

        let result: Result<&str, _> = retry_with_backoff(
            "test_operation",
            || {
                let counter = Arc::clone(&attempt_count);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Always fail with recoverable error
                    Err(anyhow::anyhow!("Network connection failed"))
                }
            },
            &context,
            false, // not autonomous mode
            3,     // max_attempts
        )
        .await;

        assert!(result.is_err());
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3); // Should try MAX_RETRY_ATTEMPTS times
    }

    #[test]
    fn test_error_context_truncation() {
        let long_prompt = "a".repeat(2000);
        let context = ErrorContext::new(
            "test_op".to_string(),
            "provider".to_string(),
            "model".to_string(),
            long_prompt,
            None,
            100,
            false,  // quiet parameter
        );

        // The prompt should be truncated to 1000 chars
        assert!(context.last_prompt.len() < 1100); // Some buffer for the truncation message
        assert!(context.last_prompt.contains("truncated"));
    }

    #[test]
    fn test_retry_delay_increases() {
        let delay1 = calculate_retry_delay(1, false);
        let delay2 = calculate_retry_delay(2, false);
        let delay3 = calculate_retry_delay(3, false);

        // Delays should generally increase (though jitter can affect this)
        // We'll test the base delays without jitter
        let base1 = 1000u64; // BASE_RETRY_DELAY_MS
        let base2 = 1000u64 * 2;
        let base3 = 1000u64 * 4;

        // Check that delays are within expected ranges (accounting for jitter)
        assert!(delay1.as_millis() >= (base1 as f64 * 0.7) as u128);
        assert!(delay1.as_millis() <= (base1 as f64 * 1.3) as u128);

        assert!(delay2.as_millis() >= (base2 as f64 * 0.7) as u128);
        assert!(delay2.as_millis() <= (base2 as f64 * 1.3) as u128);

        assert!(delay3.as_millis() >= (base3 as f64 * 0.7) as u128);
        assert!(delay3.as_millis() <= (base3 as f64 * 1.3) as u128);
    }
}
