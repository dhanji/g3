#[cfg(test)]
mod gitignore_prompt_tests {
    use crate::Agent;
    use crate::ui_writer::UiWriter;

    // Mock UI writer for testing
    struct MockUiWriter;

    impl UiWriter for MockUiWriter {
        fn print_agent_prompt(&self) {}
        fn print_agent_response(&self, _text: &str) {}
        fn print(&self, _message: &str) {}
        fn print_inline(&self, _message: &str) {}
        fn print_tool_output_line(&self, _line: &str) {}
        fn print_system_prompt(&self, _text: &str) {}
        fn print_tool_header(&self, _tool_name: &str) {}
        fn print_tool_arg(&self, _key: &str, _value: &str) {}
        fn print_tool_output_header(&self) {}
        fn update_tool_output_line(&self, _line: &str) {}
        fn print_tool_output_summary(&self, _total_lines: usize) {}
        fn print_tool_timing(&self, _duration: &str) {}
        fn print_context_status(&self, _message: &str) {}
        fn print_context_thinning(&self, _message: &str) {}
        fn println(&self, _text: &str) {}
        fn flush(&self) {}
        fn notify_sse_received(&self) {}
        fn wants_full_output(&self) -> bool { false }
    }

    #[test]
    fn test_gitignore_prompt_snippet_with_file() {
        // Create a temporary .gitignore file
        let test_gitignore = "# Test comment\ntarget/\n*.log\n\n# Another comment\nlogs/\n";
        std::fs::write(".gitignore.test", test_gitignore).unwrap();

        // Temporarily rename actual .gitignore if it exists
        let has_real_gitignore = std::path::Path::new(".gitignore").exists();
        if has_real_gitignore {
            std::fs::rename(".gitignore", ".gitignore.backup").unwrap();
        }

        // Rename test file to .gitignore
        std::fs::rename(".gitignore.test", ".gitignore").unwrap();

        let snippet = Agent::<MockUiWriter>::get_gitignore_prompt_snippet();

        // Restore original .gitignore
        std::fs::remove_file(".gitignore").unwrap();
        if has_real_gitignore {
            std::fs::rename(".gitignore.backup", ".gitignore").unwrap();
        }

        assert!(snippet.contains("IMPORTANT"));
        assert!(snippet.contains(".gitignore"));
        assert!(snippet.contains("target/"));
        assert!(snippet.contains("*.log"));
    }

    #[test]
    fn test_gitignore_prompt_snippet_without_file() {
        // Temporarily rename .gitignore if it exists
        let has_gitignore = std::path::Path::new(".gitignore").exists();
        if has_gitignore {
            std::fs::rename(".gitignore", ".gitignore.backup").unwrap();
        }

        let snippet = Agent::<MockUiWriter>::get_gitignore_prompt_snippet();

        // Restore .gitignore
        if has_gitignore {
            std::fs::rename(".gitignore.backup", ".gitignore").unwrap();
        }

        assert_eq!(snippet, "");
    }
}
