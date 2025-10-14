// New implementation of filter_json_tool_calls function
// This replaces the broken implementation with a correct one according to the specification

use std::cell::RefCell;
use regex::Regex;
use tracing::debug;

// Thread-local state for tracking JSON tool call suppression
thread_local! {
    static NEW_JSON_TOOL_STATE: RefCell<NewJsonToolState> = RefCell::new(NewJsonToolState::new());
}

#[derive(Debug, Clone)]
struct NewJsonToolState {
    suppression_mode: bool,
    brace_depth: i32,
    accumulated_content: String,
    json_start_pos: Option<usize>,
}

impl NewJsonToolState {
    fn new() -> Self {
        Self {
            suppression_mode: false,
            brace_depth: 0,
            accumulated_content: String::new(),
            json_start_pos: None,
        }
    }

    fn reset(&mut self) {
        self.suppression_mode = false;
        self.brace_depth = 0;
        self.accumulated_content.clear();
        self.json_start_pos = None;
    }
}

// New implementation according to specification:
// 1. Detect tool call start with regex '\w*{\w*"tool"\w*:\w*"' on the very next newline
// 2. Enter suppression mode and use brace counting to find complete JSON
// 3. Only elide JSON content between first '{' and last '}' (inclusive)
// 4. Return everything else as the final filtered string
pub fn new_filter_json_tool_calls(content: &str) -> String {
    NEW_JSON_TOOL_STATE.with(|state| {
        let mut state = state.borrow_mut();
        
        // Always accumulate content for processing
        let content_start_pos = state.accumulated_content.len();
        state.accumulated_content.push_str(content);

        // If we're already in suppression mode, continue brace counting
        if state.suppression_mode {
            // Count braces in the new content to track JSON completion
            for ch in content.chars() {
                match ch {
                    '{' => state.brace_depth += 1,
                    '}' => {
                        state.brace_depth -= 1;
                        // Exit suppression mode when all braces are closed
                        if state.brace_depth <= 0 {
                            debug!("JSON tool call completed - exiting suppression mode");
                            
                            // Extract the complete result with JSON filtered out
                            let result = extract_filtered_content(&state.accumulated_content, state.json_start_pos.unwrap_or(0));
                            state.reset();
                            return result;
                        }
                    }
                    _ => {}
                }
            }
            // Still in suppression mode, return empty string
            return String::new();
        }

        // Check for tool call pattern - the specification requires:
        // '\w*{\w*"tool"\w*:\w*"' on the very next newline
        // However, based on our analysis, we need to be more flexible with whitespace
        // The original regex was too strict and didn't account for spaces properly
        let tool_call_regex = Regex::new(r#"(?m)^.*\{\s*"tool"\s*:\s*""#).unwrap();
        
        if let Some(captures) = tool_call_regex.find(&state.accumulated_content) {
            let match_start = captures.start();
            let match_text = captures.as_str();
            
            // Find the position of the opening brace in the match
            if let Some(brace_offset) = match_text.find('{') {
                let json_start = match_start + brace_offset;
                
                debug!("Detected JSON tool call at position {} - entering suppression mode", json_start);
                
                // Enter suppression mode
                state.suppression_mode = true;
                state.brace_depth = 0;
                state.json_start_pos = Some(json_start);
                
                // Count braces from the JSON start to see if it's complete
                // Clone the content to avoid borrow checker issues
                let accumulated_content = state.accumulated_content.clone();
                for ch in accumulated_content[json_start..].chars() {
                    match ch {
                        '{' => state.brace_depth += 1,
                        '}' => {
                            state.brace_depth -= 1;
                            if state.brace_depth <= 0 {
                                // JSON is complete in this chunk
                                debug!("JSON tool call completed in same chunk");
                                let result = extract_filtered_content(&accumulated_content, json_start);
                                state.reset();
                                return result;
                            }
                        }
                        _ => {}
                    }
                }
                
                // JSON is incomplete, return content before the JSON start
                // But only return the new content that was added before the JSON
                if json_start > content_start_pos {
                    // JSON starts in the new content
                    let new_content_before_json = json_start - content_start_pos;
                    return content[..new_content_before_json].to_string();
                } else {
                    // JSON started in previous content, return empty
                    return String::new();
                }
            }
        }

        // No JSON tool call detected - return only the new content, not accumulated
        // This prevents duplication in streaming scenarios
        content.to_string()
    })
}

// Helper function to extract content with JSON tool call filtered out
// Returns everything except the JSON between the first '{' and last '}' (inclusive)
fn extract_filtered_content(full_content: &str, json_start: usize) -> String {
    // Find the end of the JSON using proper brace counting
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

// Reset function for testing
pub fn reset_new_json_tool_state() {
    NEW_JSON_TOOL_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.reset();
    });
}