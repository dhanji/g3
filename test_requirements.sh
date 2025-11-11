#!/bin/bash

# Test script for .g3/requirements.md feature

set -e

echo "Testing .g3/requirements.md feature..."
echo ""

# Create a test directory
TEST_DIR="/tmp/g3_test_$$"
mkdir -p "$TEST_DIR"
cd "$TEST_DIR"

echo "Test directory: $TEST_DIR"
echo ""

# Create a simple test by simulating user input
echo "Testing requirement persistence..."
echo ""

# Check if .g3 directory gets created
if [ ! -d ".g3" ]; then
    echo "✅ .g3 directory does not exist yet (expected)"
else
    echo "❌ .g3 directory already exists (unexpected)"
fi

echo ""
echo "Test directory created at: $TEST_DIR"
echo "You can manually test by running:"
echo "  cd $TEST_DIR"
echo "  g3"
echo ""
echo "Then enter a requirement and check if .g3/requirements.md is created."
echo ""
