//! Shared utilities for streaming SSE response parsing.
//!
//! This module provides common helpers used by multiple LLM providers
//! for handling Server-Sent Events (SSE) streaming responses.

use crate::{CompletionChunk, ToolCall, Usage};

// ─────────────────────────────────────────────────────────────────────────────
// UTF-8 Streaming
// ─────────────────────────────────────────────────────────────────────────────

/// Try to decode bytes as UTF-8, handling incomplete sequences at the end.
/// Returns the decoded string and leaves any incomplete bytes in the buffer.
pub fn decode_utf8_streaming(byte_buffer: &mut Vec<u8>) -> Option<String> {
    match std::str::from_utf8(byte_buffer) {
        Ok(s) => {
            let result = s.to_string();
            byte_buffer.clear();
            Some(result)
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            if valid_up_to > 0 {
                let valid_bytes: Vec<u8> = byte_buffer.drain(..valid_up_to).collect();
                // Safe: we just validated these bytes
                Some(String::from_utf8(valid_bytes).unwrap())
            } else {
                None // No valid UTF-8 yet, wait for more bytes
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON Error Detection
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a JSON parse error indicates incomplete data (vs. malformed JSON).
pub fn is_incomplete_json_error(error: &serde_json::Error, data: &str) -> bool {
    let msg = error.to_string().to_lowercase();
    let looks_incomplete = msg.contains("eof")
        || msg.contains("unterminated")
        || msg.contains("unexpected end")
        || msg.contains("trailing");
    let missing_terminator = !data.trim_end().ends_with('}') && !data.trim_end().ends_with(']');
    looks_incomplete || missing_terminator
}

// ─────────────────────────────────────────────────────────────────────────────
// Completion Chunk Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Create a final completion chunk with tool calls and usage.
pub fn make_final_chunk(tool_calls: Vec<ToolCall>, usage: Option<Usage>) -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: true,
        usage,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    }
}

/// Create a final completion chunk with stop reason.
pub fn make_final_chunk_with_reason(tool_calls: Vec<ToolCall>, usage: Option<Usage>, stop_reason: Option<String>) -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: true,
        usage,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        stop_reason,
        tool_call_streaming: None,
        upstream_ping: false,
    }
}

/// Create a text content chunk (not finished).
pub fn make_text_chunk(content: String) -> CompletionChunk {
    CompletionChunk {
        content,
        finished: false,
        usage: None,
        tool_calls: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    }
}

/// Create a tool calls chunk (not finished).
pub fn make_tool_chunk(tool_calls: Vec<ToolCall>) -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: false,
        usage: None,
        tool_calls: Some(tool_calls),
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: false,
    }
}

/// Create a hint chunk indicating a tool call is being streamed.
pub fn make_tool_streaming_hint(tool_name: String) -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: false,
        usage: None,
        tool_calls: None,
        stop_reason: None,
        tool_call_streaming: Some(tool_name),
        upstream_ping: false,
    }
}

/// Create a hint chunk indicating a tool call is still actively streaming.
/// This is used to trigger UI updates (like blinking indicators) during long tool calls.
pub fn make_tool_streaming_active() -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: false,
        usage: None,
        tool_calls: None,
        stop_reason: None,
        tool_call_streaming: Some(String::new()), // Empty string signals "active" vs "detected"
        upstream_ping: false,
    }
}

/// Create an upstream keep-alive chunk from a provider `ping` frame.
///
/// Carries NO content on purpose — see `CompletionChunk::upstream_ping`. The
/// whole value of this chunk is its arrival time, which is the only evidence
/// available that the upstream is still working during a long think.
pub fn make_upstream_ping_chunk() -> CompletionChunk {
    CompletionChunk {
        content: String::new(),
        finished: false,
        usage: None,
        tool_calls: None,
        stop_reason: None,
        tool_call_streaming: None,
        upstream_ping: true,
    }
}
