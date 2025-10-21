#!/bin/bash

# Test script to verify token accounting with Anthropic provider

echo "Testing token accounting with Anthropic provider..."
echo "This test will send a few messages and check if token counts are properly tracked."
echo ""

# Set up environment for testing
export RUST_LOG=g3_providers=debug,g3_core=info
export RUST_BACKTRACE=1

# Build the project first
echo "Building project..."
cargo build --release 2>&1 | grep -E "(Compiling|Finished)" || true

echo ""
echo "Running test with Anthropic provider..."
echo "Watch for these log messages:"
echo "  - 'Captured initial usage from message_start'"
echo "  - 'Updated usage from message_delta' (if available)"
echo "  - 'Updated with final usage from message_stop' (if available)"
echo "  - 'Anthropic stream completed with final usage'"
echo "  - 'Updated token usage from provider'"
echo "  - 'Context thinning triggered' (when reaching thresholds)"
echo ""

# Create a simple test that will generate some tokens
cat << 'EOF' > /tmp/test_prompt.txt
Please write a short paragraph about the importance of accurate token counting in LLM applications. Then list 3 reasons why token accounting might fail.
EOF

# Run the test
echo "Sending test prompt..."
cargo run --release -- --provider anthropic "$(cat /tmp/test_prompt.txt)" 2>&1 | tee /tmp/token_test.log

echo ""
echo "Analyzing results..."
echo ""

# Check for token accounting messages
echo "Token accounting messages found:"
grep -E "(usage from|token usage|Context thinning|Context usage)" /tmp/token_test.log | head -20

echo ""
echo "Test complete. Check /tmp/token_test.log for full output."