# Anthropic max_tokens Error Fix - Test Plan

## Changes Made

### 1. Fixed Context Window Size Detection
- **Problem**: Code used hardcoded 200k limit for Anthropic instead of configured max_tokens
- **Fix**: Modified `determine_context_length()` to check configured max_tokens first before falling back to defaults
- **Files**: `crates/g3-core/src/lib.rs` lines 923-945, 967-985

### 2. Added Thinning Before Summarization
- **Problem**: Code attempted summarization even when context window was nearly full
- **Fix**: Added logic to try thinning first when context usage is between 80-90%
- **Files**: `crates/g3-core/src/lib.rs` lines 2415-2439

### 3. Added Capacity Checks Before Summarization
- **Problem**: No validation that sufficient tokens remained for summarization
- **Fix**: Added capacity checks for all provider types with helpful error messages
- **Files**: `crates/g3-core/src/lib.rs` lines 2480-2520

### 4. Improved Error Messages
- **Problem**: Generic errors when summarization failed
- **Fix**: Specific error messages suggesting `/thinnify` and `/compact` commands
- **Files**: Multiple locations in summarization logic

### 5. Dynamic Buffer Calculation
- **Problem**: Fixed 5k buffer regardless of model size
- **Fix**: Proportional buffer (2.5% of model limit, min 1k, max 10k)
- **Files**: `crates/g3-core/src/lib.rs` line 2487

## Test Cases

### Test 1: Configured max_tokens Respected
```toml
# In g3.toml
[providers.anthropic]
api_key = "your-key"
model = "claude-3-5-sonnet-20241022"
max_tokens = 50000  # Should use this instead of 200k default
```

### Test 2: Thinning Before Summarization
- Fill context to 85% capacity
- Verify thinning is attempted before summarization
- Check that summarization is skipped if thinning resolves the issue

### Test 3: Capacity Error Handling
- Fill context to 98% capacity
- Verify helpful error message is shown instead of API error
- Check that `/thinnify` and `/compact` commands are suggested

### Test 4: Provider-Specific Handling
- Test with different providers (anthropic, databricks, embedded)
- Verify each uses appropriate capacity checks and buffers

## Expected Behavior

1. **No more max_tokens API errors** from Anthropic when context window is full
2. **Automatic thinning** when approaching capacity (80-90%)
3. **Clear error messages** with actionable suggestions when at capacity
4. **Respect configured limits** instead of hardcoded defaults
5. **Graceful degradation** with helpful user guidance

## Manual Testing Commands

```bash
# Test with small max_tokens to trigger the issue quickly
g3 --chat
# Then paste large amounts of text to fill context window
# Verify thinning and error handling work correctly
```
