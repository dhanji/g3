use std::cell::RefCell;
use regex::Regex;
use tracing::debug;

// Thread-local state for tracking JSON tool call suppression
thread_local! {
    static JSON_TOOL_STATE: RefCell<JsonToolState> = RefCell::new(JsonToolState::new());
}

#[derive(Debug, Clone)]
struct JsonToolState {
    suppression_mode: bool,
    brace_depth: i32,
    accumulated_content: String,
    json_start_pos: Option<usize>,
}

impl JsonToolState {
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

// Helper function to filter JSON tool calls from display content
// Implementation according to specification:
// 1. Detect tool call start with regex '\w*{\w*"tool"\w*:\w*"' on the very next newline
// 2. Enter suppression mode and use brace counting to find complete JSON
// 3. Only elide JSON content between first '{' and last '}' (inclusive)
// 4. Return everything else as the final filtered string
pub fn filter_json_tool_calls(content: &str) -> String {
    JSON_TOOL_STATE.with(|state| {
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

        // Check for tool call pattern using the specified regex: \w*{\w*"tool"\w*:\w*"
        // We need to check if this pattern appears on a newline
        let tool_call_regex = Regex::new(r#"(?m)^.*\w*\{\w*"tool"\w*:\w*""#).unwrap();
        
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
                for ch in state.accumulated_content[json_start..].chars() {
                    match ch {
                        '{' => state.brace_depth += 1,
                        '}' => {
                            state.brace_depth -= 1;
                            if state.brace_depth <= 0 {
                                // JSON is complete in this chunk
                                debug!("JSON tool call completed in same chunk");
                                let result = extract_filtered_content(&state.accumulated_content, json_start);
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

        // No JSON tool call detected, return the new content as-is
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
pub fn reset_json_tool_state() {
    JSON_TOOL_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.reset();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_tool_call_passthrough() {
        reset_json_tool_state();
        let input = "This is regular text without any tool calls.";
        let result = filter_json_tool_calls(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_simple_tool_call_detection() {
        reset_json_tool_state();
        let input = r#"Some text before
{"tool": "shell", "args": {"command": "ls"}}
Some text after"#;
        
        let result = filter_json_tool_calls(input);
        let expected = "Some text before\n\nSome text after";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_tool_call_at_start_of_newline() {
        reset_json_tool_state();
        let input = "Previous text\n{\"tool\": \"read_file\", \"args\": {\"file_path\": \"test.txt\"}}\nNext text";
        
        let result = filter_json_tool_calls(input);
        let expected = "Previous text\n\nNext text";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_streaming_chunks() {
        reset_json_tool_state();
        
        // Simulate streaming where the tool call comes in multiple chunks
        let chunks = vec![
            "Some text before\n",
            "{\"tool\": \"",
            "shell\", \"args\": {",
            "\"command\": \"ls\"",
            "}}\nText after"
        ];
        
        let mut results = Vec::new();
        for chunk in chunks {
            let result = filter_json_tool_calls(chunk);
            results.push(result);
        }
        
        // The final accumulated result should have the JSON filtered out
        let final_result: String = results.join("");
        let expected = "Some text before\n\nText after";
        assert_eq!(final_result, expected);
    }

    #[test]
    fn test_nested_braces_in_tool_call() {
        reset_json_tool_state();
        
        let input = r#"Text before
{"tool": "write_file", "args": {"file_path": "test.json", "content": "{\"nested\": \"value\"}"}}
Text after"#;
        
        let result = filter_json_tool_calls(input);
        let expected = "Text before\n\nText after";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_multiple_tool_calls() {
        reset_json_tool_state();
        
        let input = r#"First text
{"tool": "shell", "args": {"command": "ls"}}
Middle text
{"tool": "read_file", "args": {"file_path": "test.txt"}}
Final text"#;
        
        // Process first tool call
        let result1 = filter_json_tool_calls(input);
        
        // For multiple tool calls in one input, we need to process iteratively
        // This is a limitation of the current design - it processes one tool call at a time
        let expected_first_pass = "First text\n\nMiddle text\n{\"tool\": \"read_file\", \"args\": {\"file_path\": \"test.txt\"}}\nFinal text";
        assert_eq!(result1, expected_first_pass);
    }

    #[test]
    fn test_regex_pattern_specification() {
        // Test the exact regex pattern specified: \w*{\w*"tool"\w*:\w*"
        let pattern = Regex::new(r#"\w*\{\w*"tool"\w*:\w*""#).unwrap();
        
        let test_cases = vec![
            (r#"{"tool":"#, true),
            (r#"{"tool" :"#, true),
            (r#"{ "tool":"#, false), // Space before { should not match \w*
            (r#"abc{"tool":"#, true),
            (r#"{"tool123":"#, false), // "tool123" is not exactly "tool"
            (r#"{"tool" : "#, true),
        ];
        
        for (input, should_match) in test_cases {
            let matches = pattern.is_match(input);
            assert_eq!(matches, should_match, "Pattern matching failed for: {}", input);
        }
    }

    #[test]
    fn test_newline_requirement() {
        reset_json_tool_state();
        
        // According to spec, tool call should be detected "on the very next newline"
        let input_with_newline = "Text\n{\"tool\": \"shell\", \"args\": {\"command\": \"ls\"}}";
        let input_without_newline = "Text {\"tool\": \"shell\", \"args\": {\"command\": \"ls\"}}";
        
        let result1 = filter_json_tool_calls(input_with_newline);
        reset_json_tool_state();
        let result2 = filter_json_tool_calls(input_without_newline);
        
        // With newline should trigger suppression
        assert_eq!(result1, "Text\n");
        // Without newline should pass through unchanged
        assert_eq!(result2, input_without_newline);
    }

    #[test]
    fn test_json_with_escaped_quotes() {
        reset_json_tool_state();
        
        let input = r#"Text
{"tool": "write_file", "args": {"content": "He said \"hello\" to me"}}
More text"#;
        
        let result = filter_json_tool_calls(input);
        let expected = "Text\n\nMore text";
        assert_eq!(result, expected);
    }
}