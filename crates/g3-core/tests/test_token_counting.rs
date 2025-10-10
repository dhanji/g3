use g3_core::ContextWindow;
use g3_providers::Usage;

#[test]
fn test_token_accumulation() {
    let mut window = ContextWindow::new(10000);
    
    // First API call: 100 prompt + 50 completion = 150 total
    let usage1 = Usage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    window.update_usage_from_response(&usage1);
    assert_eq!(window.used_tokens, 150, "First call should have 150 tokens");
    assert_eq!(window.cumulative_tokens, 150, "Cumulative should be 150");

    // Second API call: 200 prompt + 75 completion = 275 total
    let usage2 = Usage {
        prompt_tokens: 200,
        completion_tokens: 75,
        total_tokens: 275,
    };
    window.update_usage_from_response(&usage2);
    assert_eq!(window.used_tokens, 425, "Second call should accumulate to 425 tokens");
    assert_eq!(window.cumulative_tokens, 425, "Cumulative should be 425");

    // Third API call with SMALLER token count: 50 prompt + 25 completion = 75 total
    let usage3 = Usage {
        prompt_tokens: 50,
        completion_tokens: 25,
        total_tokens: 75,
    };
    window.update_usage_from_response(&usage3);
    assert_eq!(window.used_tokens, 500, "Third call should accumulate to 500 tokens");
    assert_eq!(window.cumulative_tokens, 500, "Cumulative should be 500");
    
    // Verify tokens never decrease
    assert!(window.used_tokens >= 425, "Token count should never decrease!");
}

#[test]
fn test_add_streaming_tokens() {
    let mut window = ContextWindow::new(10000);
    
    // Add some streaming tokens
    window.add_streaming_tokens(100);
    assert_eq!(window.used_tokens, 100);
    assert_eq!(window.cumulative_tokens, 100);
    
    // Add more
    window.add_streaming_tokens(50);
    assert_eq!(window.used_tokens, 150);
    assert_eq!(window.cumulative_tokens, 150);
    
    // Now update from provider response
    let usage = Usage {
        prompt_tokens: 80,
        completion_tokens: 40,
        total_tokens: 120,
    };
    window.update_usage_from_response(&usage);
    
    // Should ADD to existing, not replace
    assert_eq!(window.used_tokens, 270, "Should add 120 to existing 150");
    assert_eq!(window.cumulative_tokens, 270);
}

#[test]
fn test_percentage_calculation() {
    let mut window = ContextWindow::new(1000);
    
    // Add tokens via provider response
    let usage = Usage {
        prompt_tokens: 150,
        completion_tokens: 100,
        total_tokens: 250,
    };
    window.update_usage_from_response(&usage);
    
    assert_eq!(window.percentage_used(), 25.0);
    assert_eq!(window.remaining_tokens(), 750);
    
    // Add more tokens
    let usage2 = Usage {
        prompt_tokens: 300,
        completion_tokens: 200,
        total_tokens: 500,
    };
    window.update_usage_from_response(&usage2);
    
    assert_eq!(window.percentage_used(), 75.0);
    assert_eq!(window.remaining_tokens(), 250);
}
