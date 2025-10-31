// FINAL CORRECTED implementation of filter_json_tool_calls function according to specification
// 1. Detect tool call start with regex '\w*{\w*"tool"\w*:\w*"' on the very next newline
// 2. Enter suppression mode and use brace counting to find complete JSON
// 3. Only elide JSON content between first '{' and last '}' (inclusive)
// 4. Return everything else as the final filtered string

//! JSON tool call filtering for streaming LLM responses.
//! 
//! This module filters out JSON tool calls from LLM output streams while preserving
//! regular text content. It uses a state machine to handle streaming chunks.

use regex::Regex;
use std::cell::RefCell;
use tracing::debug;

// Thread-local state for tracking JSON tool call suppression
thread_local! {
    static FIXED_JSON_TOOL_STATE: RefCell<FixedJsonToolState> = RefCell::new(FixedJsonToolState::new());
}

/// Internal state for tracking JSON tool call filtering across streaming chunks.
#[derive(Debug, Clone)]
struct FixedJsonToolState {
    /// True when actively suppressing a confirmed tool call
    suppression_mode: bool,
    /// True when buffering potential JSON (saw { but not yet confirmed as tool call)
    potential_json_mode: bool,
    /// Tracks nesting depth of braces within JSON
    brace_depth: i32,
    buffer: String,
    json_start_in_buffer: Option<usize>, // Position where confirmed JSON tool call starts
    content_returned_up_to: usize, // Track how much content we've already returned
    potential_json_start: Option<usize>, // Where the potential JSON started
}

impl FixedJsonToolState {
    fn new() -> Self {
        Self {
            suppression_mode: false,
            potential_json_mode: false,
            brace_depth: 0,
            buffer: String::new(),
            json_start_in_buffer: None,
            content_returned_up_to: 0,
            potential_json_start: None,
        }
    }

    fn reset(&mut self) {
        self.suppression_mode = false;
        self.potential_json_mode = false;
        self.brace_depth = 0;
        self.buffer.clear();
        self.json_start_in_buffer = None;
        self.content_returned_up_to = 0;
        self.potential_json_start = None;
    }
}

// FINAL CORRECTED implementation according to specification

/// Filters JSON tool calls from streaming LLM content.
///
/// Processes content chunks and removes JSON tool calls while preserving regular text.
/// Maintains state across calls to handle tool calls spanning multiple chunks.
pub fn fixed_filter_json_tool_calls(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }

    FIXED_JSON_TOOL_STATE.with(|state| {
        let mut state = state.borrow_mut();

        // Add new content to buffer
        state.buffer.push_str(content);

        // If we're already in suppression mode, continue brace counting
        if state.suppression_mode {
            // Count braces in the new content only
            for ch in content.chars() {
                match ch {
                    '{' => state.brace_depth += 1,
                    '}' => {
                        state.brace_depth -= 1;
                        // Exit suppression mode when all braces are closed
                        if state.brace_depth <= 0 {
                            debug!("JSON tool call completed - exiting suppression mode");

                            // Extract the complete result with JSON filtered out
                            let result = extract_fixed_content(
                                &state.buffer,
                                state.json_start_in_buffer.unwrap_or(0),
                            );

                            // Return only the part we haven't returned yet
                            let new_content = if result.len() > state.content_returned_up_to {
                                result[state.content_returned_up_to..].to_string()
                            } else {
                                String::new()
                            };

                            state.reset();
                            return new_content;
                        }
                    }
                    _ => {}
                }
            }
            // Still in suppression mode, return empty string (content is being accumulated)
            return String::new();
        }

        // Check if we're in potential JSON mode (saw { but waiting to confirm it's a tool call)
        if state.potential_json_mode {
            // Check if the buffer contains a confirmed tool call pattern
            let tool_call_regex = Regex::new(r#"(?m)^\s*\{\s*"tool"\s*:\s*""#).unwrap();
            
            if let Some(captures) = tool_call_regex.find(&state.buffer) {
                // Confirmed! This is a tool call - enter suppression mode
                let match_text = captures.as_str();
                if let Some(brace_offset) = match_text.find('{') {
                    let json_start = captures.start() + brace_offset;
                    
                    debug!("Confirmed JSON tool call at position {} - entering suppression mode", json_start);
                    
                    state.potential_json_mode = false;
                    state.suppression_mode = true;
                    state.brace_depth = 0;
                    state.json_start_in_buffer = Some(json_start);
                    
                    // Count braces from json_start to see if JSON is complete
                    let buffer_slice = state.buffer[json_start..].to_string();
                    for ch in buffer_slice.chars() {
                        match ch {
                            '{' => state.brace_depth += 1,
                            '}' => {
                                state.brace_depth -= 1;
                                if state.brace_depth <= 0 {
                                    debug!("JSON tool call completed immediately");
                                    let result = extract_fixed_content(&state.buffer, json_start);
                                    let new_content = if result.len() > state.content_returned_up_to {
                                        result[state.content_returned_up_to..].to_string()
                                    } else {
                                        String::new()
                                    };
                                    state.reset();
                                    return new_content;
                                }
                            }
                            _ => {}
                        }
                    }
                    // JSON incomplete, stay in suppression mode, return nothing
                    return String::new();
                }
            }
            
            // Check if we can rule out this being a tool call
            // If we have enough content after the { and it doesn't match the pattern, release it
            if let Some(potential_start) = state.potential_json_start {
                let content_after_brace = &state.buffer[potential_start..];
                
                // Rule out as a tool call if:
                // 1. Closing } appears before we see the full pattern
                // 2. Content clearly doesn't match the tool call pattern
                // 3. Newline appears after the opening brace (tool calls should be compact)
                
                let has_closing_brace = content_after_brace.contains('}');
                let has_newline = content_after_brace[1..].contains('\n'); // Skip first char which is {
                let long_enough = content_after_brace.len() >= 10;
                
                // Detect non-tool JSON patterns:
                // - { followed by " and a key that doesn't start with "tool"
                // - { followed by "t" but not "to"
                // - { followed by "to" but not "too", etc.
                let not_tool_pattern = Regex::new(r#"^\{\s*"(?:[^t]|t(?:[^o]|o(?:[^o]|o(?:[^l]|l[^"\s:]))))"#).unwrap();
                let definitely_not_tool = not_tool_pattern.is_match(content_after_brace);
                
                if has_closing_brace || has_newline || (long_enough && definitely_not_tool) {
                    debug!("Potential JSON ruled out - not a tool call");
                    state.potential_json_mode = false;
                    state.potential_json_start = None;
                    
                    // Return the buffered content we've been holding
                    let new_content = if state.buffer.len() > state.content_returned_up_to {
                        state.buffer[state.content_returned_up_to..].to_string()
                    } else {
                        String::new()
                    };
                    state.content_returned_up_to = state.buffer.len();
                    return new_content;
                }
            }
            
            // Still in potential mode, keep buffering
            return String::new();
        }

        // Detect potential JSON start: { at the beginning of a line
        let potential_json_regex = Regex::new(r"(?m)^\s*\{\s*").unwrap();
        
        if let Some(captures) = potential_json_regex.find(&state.buffer[state.content_returned_up_to..]) {
            let match_start = state.content_returned_up_to + captures.start();
            let brace_pos = match_start + captures.as_str().find('{').unwrap();
            
            debug!("Potential JSON detected at position {} - entering buffering mode", brace_pos);
            
            // Fast path: check if this is already a confirmed tool call
            let tool_call_regex = Regex::new(r#"(?m)^\s*\{\s*"tool"\s*:\s*""#).unwrap();
            if tool_call_regex.is_match(&state.buffer[brace_pos..]) {
                // This is a confirmed tool call! Process it immediately
                let json_start = brace_pos;
                debug!("Immediately confirmed tool call at position {}", json_start);
                
                // Return content before JSON
                let content_before = if json_start > state.content_returned_up_to {
                    state.buffer[state.content_returned_up_to..json_start].to_string()
                } else {
                    String::new()
                };
                
                state.content_returned_up_to = json_start;
                state.suppression_mode = true;
                state.brace_depth = 0;
                state.json_start_in_buffer = Some(json_start);
                
                // Count braces to see if JSON is complete
                let buffer_slice = state.buffer[json_start..].to_string();
                for ch in buffer_slice.chars() {
                    match ch {
                        '{' => state.brace_depth += 1,
                        '}' => {
                            state.brace_depth -= 1;
                            if state.brace_depth <= 0 {
                                debug!("JSON tool call completed in same chunk");
                                let result = extract_fixed_content(&state.buffer, json_start);
                                let content_after = if result.len() > json_start {
                                    &result[json_start..]
                                } else {
                                    ""
                                };
                                let final_result = format!("{}{}", content_before, content_after);
                                state.reset();
                                return final_result;
                            }
                        }
                        _ => {}
                    }
                }
                // JSON incomplete, return content before and stay in suppression mode
                return content_before;
            }
            
            // Return content before the potential JSON
            let content_before = if brace_pos > state.content_returned_up_to {
                state.buffer[state.content_returned_up_to..brace_pos].to_string()
            } else {
                String::new()
            };
            
            state.content_returned_up_to = brace_pos;
            state.potential_json_mode = true;
            state.potential_json_start = Some(brace_pos);
            
            // Optimization: immediately check if we can rule this out for single-chunk processing
            let content_after_brace = &state.buffer[brace_pos..];
            let has_closing_brace = content_after_brace.contains('}');
            let has_newline = content_after_brace.len() > 1 && content_after_brace[1..].contains('\n');
            let long_enough = content_after_brace.len() >= 10;
            
            let not_tool_pattern = Regex::new(r#"^\{\s*"(?:[^t]|t(?:[^o]|o(?:[^o]|o(?:[^l]|l[^"\s:]))))"#).unwrap();
            let definitely_not_tool = not_tool_pattern.is_match(content_after_brace);
            
            if has_closing_brace || has_newline || (long_enough && definitely_not_tool) {
                debug!("Immediately ruled out as not a tool call");
                state.potential_json_mode = false;
                state.potential_json_start = None;
                
                // Return all the buffered content
                let new_content = if state.buffer.len() > state.content_returned_up_to {
                    state.buffer[state.content_returned_up_to..].to_string()
                } else {
                    String::new()
                };
                state.content_returned_up_to = state.buffer.len();
                return format!("{}{}", content_before, new_content);
            }
            
            return content_before;
        }

        // Check for tool call pattern using corrected regex
        let tool_call_regex = Regex::new(r#"(?m)^\s*\{\s*"tool"\s*:\s*"[^"]*""#).unwrap();

        if let Some(captures) = tool_call_regex.find(&state.buffer) {
            let match_text = captures.as_str();

            // Find the position of the opening brace in the match
            if let Some(brace_offset) = match_text.find('{') {
                let json_start = captures.start() + brace_offset;

                debug!(
                    "Detected JSON tool call at position {} - entering suppression mode",
                    json_start
                );

                // Return content before JSON that we haven't returned yet
                let content_before_json = if json_start >= state.content_returned_up_to {
                    state.buffer[state.content_returned_up_to..json_start].to_string()
                } else {
                    String::new()
                };

                state.content_returned_up_to = json_start;

                // Enter suppression mode
                state.suppression_mode = true;
                state.brace_depth = 0;
                state.json_start_in_buffer = Some(json_start);

                // Count braces from the JSON start to see if it's complete
                let buffer_clone = state.buffer.clone();
                for ch in buffer_clone[json_start..].chars() {
                    match ch {
                        '{' => state.brace_depth += 1,
                        '}' => {
                            state.brace_depth -= 1;
                            if state.brace_depth <= 0 {
                                // JSON is complete in this chunk
                                debug!("JSON tool call completed in same chunk");
                                let result = extract_fixed_content(&buffer_clone, json_start);

                                // Return content before JSON plus content after JSON
                                let content_after_json = if result.len() > json_start {
                                    &result[json_start..]
                                } else {
                                    ""
                                };

                                let final_result =
                                    format!("{}{}", content_before_json, content_after_json);
                                state.reset();
                                return final_result;
                            }
                        }
                        _ => {}
                    }
                }

                // JSON is incomplete, return only the content before JSON
                return content_before_json;
            }
        }

        // No JSON tool call detected, return only the new content we haven't returned yet
        

        if state.buffer.len() > state.content_returned_up_to {
            let result = state.buffer[state.content_returned_up_to..].to_string();
            state.content_returned_up_to = state.buffer.len();
            result
        } else {
            String::new()
        }
    })
}

/// Extracts content from buffer, removing the JSON tool call.
///
/// Given a buffer and the start position of a JSON tool call, this function:
/// 1. Extracts all content before the JSON
/// 2. Finds the end of the JSON (matching closing brace)
/// 3. Extracts all content after the JSON
/// 4. Returns the concatenation of before + after (JSON removed)
///
/// # Arguments
/// * `full_content` - The full content buffer
/// * `json_start` - Position where the JSON tool call begins
fn extract_fixed_content(full_content: &str, json_start: usize) -> String {
    // Find the end of the JSON using proper brace counting with string handling
    let mut brace_depth = 0;
    let mut json_end = json_start;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in full_content[json_start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' if !escape_next => in_string = !in_string,
            '{' if !in_string => {
                brace_depth += 1;
            }
            '}' if !in_string => {
                brace_depth -= 1;
                if brace_depth == 0 {
                    json_end = json_start + i + 1; // +1 to include the closing brace
                    break;
                }
            }
            _ => {}
        }
    }

    // Return content before and after the JSON (excluding the JSON itself)
    let before = &full_content[..json_start];
    let after = if json_end < full_content.len() {
        &full_content[json_end..]
    } else {
        ""
    };

    format!("{}{}", before, after)
}

/// Resets the global JSON filtering state.
///
/// Call this between independent filtering sessions to ensure clean state.
/// This is particularly important in tests and when starting new conversations.
pub fn reset_fixed_json_tool_state() {
    FIXED_JSON_TOOL_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.reset();
    });
}
