//! Coach feedback extraction from session logs.
//!
//! Extracts feedback from the coach agent's session logs for the coach-player loop.

use anyhow::Result;
use g3_core::{Agent, FeedbackExtractionConfig};

use crate::simple_output::SimpleOutput;
use crate::ui_writer_impl::ConsoleUiWriter;

/// Extract coach feedback by reading from the coach agent's specific log file.
///
/// Uses the coach agent's session ID to find the exact log file.
pub fn extract_from_logs(
    coach_result: &g3_core::TaskResult,
    coach_agent: &Agent<ConsoleUiWriter>,
    output: &SimpleOutput,
) -> Result<String> {
    let extracted = g3_core::extract_coach_feedback(
        coach_result,
        coach_agent,
        &FeedbackExtractionConfig::default(),
    );

    if extracted.is_fallback() {
        return Err(anyhow::anyhow!(
            "Could not extract coach feedback. Coach result response length: {} chars",
            coach_result.response.len()
        ));
    }

    if let Some(session_id) = coach_agent.get_session_id() {
        output.print(&format!(
            "✅ Extracted coach feedback from session: {} ({:?})",
            session_id, extracted.source
        ));
    }

    Ok(extracted.content)
}
