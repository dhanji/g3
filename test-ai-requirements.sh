#!/bin/bash
# Test script for AI-enhanced interactive requirements mode

echo "Testing AI-enhanced interactive requirements mode..."
echo ""

# Create a test workspace
TEST_WORKSPACE="/tmp/g3-test-interactive-$(date +%s)"
mkdir -p "$TEST_WORKSPACE"

echo "Test workspace: $TEST_WORKSPACE"
echo ""

# Create sample brief input
BRIEF_INPUT="build a calculator cli in rust with basic operations"

echo "Brief input:"
echo "---"
echo "$BRIEF_INPUT"
echo "---"
echo ""

echo "This will:"
echo "1. Send brief input to AI"
echo "2. AI generates structured requirements.md"
echo "3. Show enhanced requirements"
echo "4. Prompt for confirmation (y/e/n)"
echo ""

echo "To test manually, run:"
echo "cargo run -- --autonomous --interactive-requirements --workspace $TEST_WORKSPACE"
echo ""
echo "Then type: $BRIEF_INPUT"
echo "Press Ctrl+D"
echo "Review the AI-generated requirements"
echo "Choose 'y' to proceed, 'e' to edit, or 'n' to cancel"
echo ""

echo "Test workspace will be at: $TEST_WORKSPACE"
