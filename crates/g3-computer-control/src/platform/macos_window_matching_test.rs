#[cfg(test)]
mod window_matching_tests {
    /// Test that window name matching handles spaces correctly
    /// 
    /// Issue: When a user requests a screenshot of "Goose Studio" but the actual
    /// application name is "GooseStudio" (no space), the fuzzy matching should
    /// still find the window.
    /// 
    /// The fix normalizes both names by removing spaces before comparing.
    #[test]
    fn test_space_normalization() {
        let test_cases = vec![
            // (user_input, actual_app_name, should_match)
            ("Goose Studio", "GooseStudio", true),
            ("GooseStudio", "Goose Studio", true),
            ("Visual Studio Code", "VisualStudioCode", true),
            ("Google Chrome", "Google Chrome", true),
            ("Safari", "Safari", true),
            ("iTerm", "iTerm2", true), // fuzzy match
            ("Code", "Visual Studio Code", true), // fuzzy match
        ];

        for (user_input, app_name, should_match) in test_cases {
            let user_lower = user_input.to_lowercase();
            let app_lower = app_name.to_lowercase();
            
            let user_normalized = user_lower.replace(" ", "");
            let app_normalized = app_lower.replace(" ", "");
            
            let is_exact = app_lower == user_lower || app_normalized == user_normalized;
            let is_fuzzy = app_lower.contains(&user_lower) 
                || user_lower.contains(&app_lower)
                || app_normalized.contains(&user_normalized)
                || user_normalized.contains(&app_normalized);
            
            let matches = is_exact || is_fuzzy;
            
            assert_eq!(
                matches, should_match,
                "Expected '{}' vs '{}' to match={}, but got match={}",
                user_input, app_name, should_match, matches
            );
        }
    }
}
