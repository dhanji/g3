//! Task execution with retry logic for G3 CLI.

use g3_core::error_handling::{calculate_retry_delay, classify_error, ErrorType, RecoverableError};
use g3_core::ui_writer::UiWriter;
use g3_core::Agent;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::simple_output::SimpleOutput;
use crate::g3_status::G3Status;

/// Maximum number of retry attempts for recoverable errors
const MAX_RETRIES: u32 = 3;

/// Get a human-readable name for a recoverable error type.
fn recoverable_error_name(err: &RecoverableError) -> &'static str {
    match err {
        RecoverableError::RateLimit => "rate limited",
        RecoverableError::ServerError => "server error",
        RecoverableError::NetworkError => "network error",
        RecoverableError::Timeout => "timeout",
        RecoverableError::ModelBusy => "model overloaded",
        RecoverableError::TokenLimit => "token limit",
        RecoverableError::ContextLengthExceeded => "context length exceeded",
    }
}

/// Execute a task with retry logic for recoverable errors.
/// Returns `true` if the task completed normally, `false` if cancelled by Ctrl+C.
pub async fn execute_task_with_retry<W: UiWriter>(
    agent: &mut Agent<W>,
    input: &str,
    show_prompt: bool,
    show_code: bool,
    output: &SimpleOutput,
) -> bool {
    let mut attempt = 0;

    output.print("🤔 Thinking...");

    // Create cancellation token for this request
    let cancellation_token = CancellationToken::new();
    let cancel_token_clone = cancellation_token.clone();

    loop {
        attempt += 1;

        // Execute task with cancellation support
        let execution_result = tokio::select! {
            result = agent.execute_task_with_timing_cancellable(
                input, None, false, show_prompt, show_code, true, cancellation_token.clone(), None
            ) => {
                result
            }
            _ = tokio::signal::ctrl_c() => {
                cancel_token_clone.cancel();
                output.print("\n⚠️  Operation cancelled by user (Ctrl+C)");
                return false;
            }
        };

        match execution_result {
            Ok(_) => {
                if attempt > 1 {
                    output.print(&format!("✅ Request succeeded after {} attempts", attempt));
                }
                // Response was already displayed during streaming - don't print again
                return true;
            }
            Err(e) => {
                if e.to_string().contains("cancelled") {
                    output.print("⚠️  Operation cancelled by user");
                    return false;
                }

                // Check if this is a recoverable error that we should retry
                let error_type = classify_error(&e);

                if let ErrorType::Recoverable(recoverable_error) = error_type {
                    if attempt < MAX_RETRIES {
                        // Use shared retry delay calculation (non-autonomous mode)
                        let delay = calculate_retry_delay(attempt, false);
                        let delay_secs = delay.as_secs_f64();

                        // Print error status
                        G3Status::complete(
                            recoverable_error_name(&recoverable_error),
                            crate::g3_status::Status::Error(String::new()),
                        );

                        // Print retry message (no newline, will show [done] after sleep)
                        G3Status::progress(&format!("retrying in {:.1}s ({}/{})", delay_secs, attempt, MAX_RETRIES));

                        // Wait before retrying
                        tokio::time::sleep(delay).await;
                        G3Status::done();
                        continue;
                    }
                }

                // For non-recoverable errors or after max retries
                handle_execution_error(&e, input, output, attempt);
                return true;
            }
        }
    }
}

/// Handle execution errors with detailed logging and user-friendly output.
pub fn handle_execution_error(e: &anyhow::Error, input: &str, _output: &SimpleOutput, attempt: u32) {
    // Check if this is a recoverable error type (for logging level decision)
    let error_type = classify_error(e);
    let is_recoverable = matches!(error_type, ErrorType::Recoverable(_));

    // Use debug level for recoverable errors (they're expected), error level for others
    if is_recoverable {
        debug!("Task execution failed (recoverable): {}", e);
        if attempt > 1 {
            debug!("Failed after {} attempts", attempt);
        }
    } else {
        error!("=== TASK EXECUTION ERROR ===");
        error!("Error: {}", e);
        if attempt > 1 {
            error!("Failed after {} attempts", attempt);
        }

        // Log error chain only for non-recoverable errors
        let mut source = e.source();
        let mut depth = 1;
        while let Some(err) = source {
            error!("  Caused by [{}]: {}", depth, err);
            source = err.source();
            depth += 1;
        }

        error!("Task input: {}", input);
        error!("Error type: {}", std::any::type_name_of_val(&e));
    }

    // Display user-friendly error message using G3Status
    if let ErrorType::Recoverable(ref recoverable_error) = error_type {
        let error_name = recoverable_error_name(recoverable_error);
        G3Status::complete(error_name, crate::g3_status::Status::Failed);
    } else {
        G3Status::complete(&format!("error: {}", e), crate::g3_status::Status::Failed);
    }
}
