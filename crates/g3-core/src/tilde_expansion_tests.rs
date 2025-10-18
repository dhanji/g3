#[cfg(test)]
mod tilde_expansion_tests {
    use std::env;

    #[test]
    fn test_tilde_expansion() {
        // Test that shellexpand works
        let path_with_tilde = "~/test.txt";
        let expanded = shellexpand::tilde(path_with_tilde);
        
        // Get the actual home directory
        let home = env::var("HOME").expect("HOME environment variable not set");
        
        // Verify expansion happened
        assert_eq!(expanded.as_ref(), format!("{}/test.txt", home));
        assert!(!expanded.contains("~"));
    }

    #[test]
    fn test_tilde_expansion_with_subdirs() {
        let path_with_tilde = "~/Documents/test.txt";
        let expanded = shellexpand::tilde(path_with_tilde);
        
        let home = env::var("HOME").expect("HOME environment variable not set");
        
        assert_eq!(expanded.as_ref(), format!("{}/Documents/test.txt", home));
    }

    #[test]
    fn test_no_tilde_unchanged() {
        let path_without_tilde = "/absolute/path/test.txt";
        let expanded = shellexpand::tilde(path_without_tilde);
        
        assert_eq!(expanded.as_ref(), path_without_tilde);
    }
}
