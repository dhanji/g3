#[cfg(test)]
mod filter_json_tests {
    use crate::filter_json_tool_calls;
    use regex::Regex;

    // Test helper to reset the thread-local state between tests
    fn reset_json_tool_state() {
        use crate::JSON_TOOL_STATE;
        crate::JSON_TOOL_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.reset();
        });
    }

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
        
        // According to the spec, we should detect the tool call and filter it out
        let result = filter_json_tool_calls(input);
        
        // The current implementation is broken - let's see what it actually does
        println!("Input: {}", input);
        println!("Result: {}", result);
        
        // What we SHOULD get according to the spec:
        let expected = "Some text before\n\nSome text after";
        // But let's see what we actually get first
    }

    #[test]
    fn test_tool_call_at_start_of_newline() {
        reset_json_tool_state();
        let input = "Previous text\n{\"tool\": \"read_file\", \"args\": {\"file_path\": \"test.txt\"}}\nNext text";
        
        let result = filter_json_tool_calls(input);
        println!("Input: {}", input);
        println!("Result: {}", result);
        
        // Should return: "Previous text\n\nNext text"
    }

    #[test]
    fn test_tool_call_with_whitespace_variations() {
        reset_json_tool_state();
        
        // Test various whitespace patterns that should match the regex
        let test_cases = vec![
            r#"Text
{"tool":"shell","args":{"command":"test"}}
More text"#,
            r#"Text
{ "tool" : "shell" , "args" : { "command" : "test" } }
More text"#,
            r#"Text
  {"tool": "shell", "args": {"command": "test"}}
More text"#,
        ];
        
        for (i, input) in test_cases.iter().enumerate() {
            reset_json_tool_state();
            let result = filter_json_tool_calls(input);
            println!("Test case {}: Input: {}", i, input);
            println!("Test case {}: Result: {}", i, result);
        }
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
            println!("Chunk: {:?} -> Result: {:?}", chunk, results.last().unwrap());
        }
        
        // The final accumulated result should have the JSON filtered out
        let final_result: String = results.join("");
        println!("Final result: {}", final_result);
    }

    #[test]
    fn test_nested_braces_in_tool_call() {
        reset_json_tool_state();
        
        let input = r#"Text before
{"tool": "write_file", "args": {"file_path": "test.json", "content": "{\"nested\": \"value\"}"}}
Text after"#;
        
        let result = filter_json_tool_calls(input);
        println!("Input: {}", input);
        println!("Result: {}", result);
        
        // Should properly handle nested braces and return: "Text before\n\nText after"
    }

    #[test]
    fn test_multiple_tool_calls() {
        reset_json_tool_state();
        
        let input = r#"First text
{"tool": "shell", "args": {"command": "ls"}}
Middle text
{"tool": "read_file", "args": {"file_path": "test.txt"}}
Final text"#;
        
        let result = filter_json_tool_calls(input);
        println!("Input: {}", input);
        println!("Result: {}", result);
        
        // Should return: "First text\n\nMiddle text\n\nFinal text"
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
            (r#"{"tool123":"#, true),
            (r#"{"tool" : "#, true),
            (r#"{"toolx":"#, false), // "toolx" is not exactly "tool"
        ];
        
        for (input, should_match) in test_cases {
            let matches = pattern.is_match(input);
            println!("Pattern test: '{}' -> matches: {} (expected: {})", input, matches, should_match);
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
        
        println!("With newline: {} -> {}", input_with_newline, result1);
        println!("Without newline: {} -> {}", input_without_newline, result2);
        
        // According to spec, only the first should trigger suppression
    }

    #[test]
    fn test_edge_case_malformed_json() {
        reset_json_tool_state();
        
        // Test what happens with malformed JSON that starts like a tool call
        let input = r#"Text
{"tool": "shell", "args": {"command": "ls"
More text"#;
        
        let result = filter_json_tool_calls(input);
        println!("Malformed JSON input: {}", input);
        println!("Result: {}", result);
        
        // Should handle gracefully - either filter it all or detect it's malformed
    }

    #[test]
    fn test_json_with_escaped_quotes() {
        reset_json_tool_state();
        
        let input = r#"Text
{"tool": "write_file", "args": {"content": "He said \"hello\" to me"}}
More text"#;
        
        let result = filter_json_tool_calls(input);
        println!("Escaped quotes input: {}", input);
        println!("Result: {}", result);
        
        // Should properly handle escaped quotes in JSON strings
    }
}