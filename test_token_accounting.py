#!/usr/bin/env python3
"""
Test script to verify token accounting is working correctly with the Anthropic provider.
This script will send multiple messages and verify that token counts accumulate properly.
"""

import subprocess
import json
import re
import sys
import time

def run_g3_command(prompt, provider="anthropic"):
    """Run a g3 command and capture the output."""
    cmd = [
        "cargo", "run", "--release", "--",
        "--provider", provider,
        prompt
    ]
    
    env = {
        "RUST_LOG": "g3_providers=debug,g3_core=info",
        "RUST_BACKTRACE": "1"
    }
    
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env={**subprocess.os.environ, **env}
    )
    
    return result.stdout + result.stderr

def extract_token_info(output):
    """Extract token usage information from the output."""
    token_info = {}
    
    # Look for token usage updates
    usage_pattern = r"Updated token usage.*was: (\d+), now: (\d+).*prompt=(\d+), completion=(\d+), total=(\d+)"
    matches = re.findall(usage_pattern, output)
    if matches:
        last_match = matches[-1]
        token_info['was'] = int(last_match[0])
        token_info['now'] = int(last_match[1])
        token_info['prompt'] = int(last_match[2])
        token_info['completion'] = int(last_match[3])
        token_info['total'] = int(last_match[4])
    
    # Look for context percentage
    context_pattern = r"Context usage at (\d+)%.*\((\d+)/(\d+) tokens\)"
    matches = re.findall(context_pattern, output)
    if matches:
        last_match = matches[-1]
        token_info['percentage'] = int(last_match[0])
        token_info['used'] = int(last_match[1])
        token_info['total_context'] = int(last_match[2])
    
    # Look for thinning triggers
    thinning_pattern = r"Context thinning triggered.*usage: (\d+)%.*\((\d+)/(\d+) tokens\)"
    matches = re.findall(thinning_pattern, output)
    if matches:
        token_info['thinning_triggered'] = True
        token_info['thinning_percentage'] = int(matches[-1][0])
    
    # Look for final usage from Anthropic
    final_usage_pattern = r"Anthropic stream completed with final usage.*prompt: (\d+), completion: (\d+), total: (\d+)"
    matches = re.findall(final_usage_pattern, output)
    if matches:
        last_match = matches[-1]
        token_info['final_prompt'] = int(last_match[0])
        token_info['final_completion'] = int(last_match[1])
        token_info['final_total'] = int(last_match[2])
    
    return token_info

def main():
    print("Testing Anthropic Provider Token Accounting")
    print("="*50)
    
    # Build the project first
    print("Building project...")
    subprocess.run(["cargo", "build", "--release"], capture_output=True)
    
    # Test 1: Simple prompt
    print("\nTest 1: Simple prompt")
    print("-"*30)
    output = run_g3_command("Say 'Hello, World!' and nothing else.")
    tokens = extract_token_info(output)
    
    if tokens:
        print(f"Token usage: {tokens.get('now', 'N/A')} tokens")
        print(f"  Prompt tokens: {tokens.get('prompt', 'N/A')}")
        print(f"  Completion tokens: {tokens.get('completion', 'N/A')}")
        print(f"  Total from provider: {tokens.get('total', 'N/A')}")
        
        if 'final_total' in tokens:
            print(f"  Final total from stream: {tokens['final_total']}")
            if tokens.get('now') != tokens['final_total']:
                print(f"  ⚠️  WARNING: Mismatch between tracked ({tokens.get('now')}) and final ({tokens['final_total']})")
        
        # Check if the completion tokens are reasonable (should be small for "Hello, World!")
        if tokens.get('completion', 0) > 50:
            print(f"  ⚠️  WARNING: Completion tokens seem high for a simple response: {tokens.get('completion')}")
    else:
        print("  ❌ No token information found in output")
    
    # Test 2: Longer response
    print("\nTest 2: Longer response")
    print("-"*30)
    output = run_g3_command("Write a 3-paragraph essay about the importance of accurate token counting in LLM applications.")
    tokens = extract_token_info(output)
    
    if tokens:
        print(f"Token usage: {tokens.get('now', 'N/A')} tokens")
        print(f"  Prompt tokens: {tokens.get('prompt', 'N/A')}")
        print(f"  Completion tokens: {tokens.get('completion', 'N/A')}")
        print(f"  Total from provider: {tokens.get('total', 'N/A')}")
        
        if 'final_total' in tokens:
            print(f"  Final total from stream: {tokens['final_total']}")
            if tokens.get('now') != tokens['final_total']:
                print(f"  ⚠️  WARNING: Mismatch between tracked ({tokens.get('now')}) and final ({tokens['final_total']})")
        
        # Check if completion tokens are reasonable for a longer response
        if tokens.get('completion', 0) < 100:
            print(f"  ⚠️  WARNING: Completion tokens seem low for a 3-paragraph essay: {tokens.get('completion')}")
    else:
        print("  ❌ No token information found in output")
    
    # Test 3: Check for proper accumulation
    print("\nTest 3: Token accumulation (multiple messages)")
    print("-"*30)
    
    # First message
    output1 = run_g3_command("Count from 1 to 5.")
    tokens1 = extract_token_info(output1)
    
    # Second message (this would need to be in a conversation, but for now we test separately)
    output2 = run_g3_command("Now count from 6 to 10.")
    tokens2 = extract_token_info(output2)
    
    if tokens1 and tokens2:
        print(f"First message: {tokens1.get('now', 'N/A')} tokens")
        print(f"Second message: {tokens2.get('now', 'N/A')} tokens")
        
        # In a real conversation, tokens2['now'] should be greater than tokens1['now']
        # But since these are separate invocations, we just check they're both reasonable
        if tokens1.get('now', 0) > 0 and tokens2.get('now', 0) > 0:
            print("  ✅ Both messages have token counts")
        else:
            print("  ❌ Missing token counts")
    
    print("\n" + "="*50)
    print("Test Summary:")
    print("Check the output above for any warnings or errors.")
    print("Key things to verify:")
    print("  1. Token counts are being captured from the provider")
    print("  2. Completion tokens are reasonable for the response length")
    print("  3. No mismatch between tracked and final token counts")
    print("  4. Context thinning triggers at appropriate thresholds")

if __name__ == "__main__":
    main()