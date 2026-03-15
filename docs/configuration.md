# g3 Configuration Guide

**Last updated**: January 2025  
**Source of truth**: `crates/g3-config/src/lib.rs`, `config.example.toml`

## Purpose

This document explains how to configure g3, including provider setup, agent behavior, and optional features like WebDriver and computer control.

## Configuration File Location

g3 looks for configuration files in this order:

1. Path specified via `--config` CLI argument
2. `./g3.toml` (current directory)
3. `~/.config/g3/config.toml` (user config)
4. `~/.g3.toml` (legacy location)

If no configuration file exists, G3 creates a default one at `~/.config/g3/config.toml` on first run.

## Configuration Format

g3 uses TOML format. The configuration is organized into sections:

```toml
[providers]           # LLM provider settings
[agent]               # Agent behavior settings
[computer_control]    # Mouse/keyboard automation
[webdriver]           # Browser automation
```

## Provider Configuration

### Provider Reference Format

Providers are referenced using the format: `<provider_type>.<config_name>`

Examples:
- `anthropic.default`
- `databricks.production`
- `openai.gpt4`
- `embedded.local`

### Basic Provider Setup

```toml
[providers]
# Default provider used for all operations
default_provider = "anthropic.default"

# Optional: Different providers for different roles
# planner = "anthropic.planner"   # Planning mode
# coach = "anthropic.default"     # Code reviewer in autonomous mode
# player = "anthropic.default"    # Code implementer in autonomous mode
```

### Anthropic Configuration

```toml
[providers.anthropic.default]
api_key = "sk-ant-..."           # Required: Your Anthropic API key
model = "claude-sonnet-4-5"      # Model to use
max_tokens = 64000               # Max output tokens per request
temperature = 0.3                # Sampling temperature (0.0-1.0)
# cache_config = "ephemeral"     # Optional: Enable prompt caching
# enable_1m_context = true        # Optional: Enable 1M context (extra cost)
# thinking_budget_tokens = 10000  # Optional: Extended thinking mode
```

**Available Anthropic models**:
- `claude-sonnet-4-5` (recommended)
- `claude-opus-4-5`
- `claude-3-5-sonnet-20241022`
- `claude-3-opus-20240229`

### Databricks Configuration

```toml
[providers.databricks.default]
host = "https://your-workspace.cloud.databricks.com"  # Required
model = "databricks-claude-sonnet-4"                   # Model endpoint
max_tokens = 4096
temperature = 0.1
use_oauth = true                 # Use OAuth (recommended)
# token = "dapi..."              # Or use personal access token
```

**OAuth vs Token Authentication**:
- **OAuth** (`use_oauth = true`): Opens browser for authentication, tokens refresh automatically
- **Token** (`token = "..."`, `use_oauth = false`): Uses personal access token directly

### Gemini Configuration

```toml
[providers.gemini.default]
api_key = "your-google-api-key"  # Required: Your Google AI API key
model = "gemini-2.0-flash"       # Model to use
max_tokens = 8192
temperature = 0.7
```

**Available Gemini models**:
- `gemini-2.0-flash` (recommended)
- `gemini-1.5-pro`
- `gemini-1.5-flash`

### OpenAI Configuration

```toml
[providers.openai.default]
api_key = "sk-..."               # Required: Your OpenAI API key
model = "gpt-4-turbo"            # Model to use
max_tokens = 4096
temperature = 0.1
# base_url = "https://api.openai.com/v1"  # Optional: Custom endpoint
# reasoning_exclude = true        # Optional: send reasoning.exclude=true
```

### OpenAI-Compatible Providers

For services with OpenAI-compatible APIs (OpenRouter, Groq, Together, etc.):

```toml
[providers.openai_compatible.openrouter]
api_key = "sk-or-..."            # Provider's API key
model = "anthropic/claude-3.5-sonnet"
base_url = "https://openrouter.ai/api/v1"
max_tokens = 4096
temperature = 0.1
reasoning_exclude = true         # Optional: hide reasoning tokens from response content

[providers.openai_compatible.groq]
api_key = "gsk_..."
model = "llama-3.3-70b-versatile"
base_url = "https://api.groq.com/openai/v1"
max_tokens = 4096
temperature = 0.1
```

Reference these as `openrouter.default` or `groq.default` in `default_provider`.

`reasoning_exclude` is optional and defaults to disabled. Enable it per provider when using
reasoning-capable models where you want concise `content` output.

### Embedded (Local) Models

```toml
[providers.embedded.default]
model_path = "~/.cache/g3/models/qwen2.5-7b-instruct-q3_k_m.gguf"
model_type = "qwen"              # Model architecture
context_length = 32768           # Context window size
max_tokens = 2048                # Max output tokens
temperature = 0.1
gpu_layers = 32                  # Layers to offload to GPU (Metal/CUDA)
threads = 8                      # CPU threads for inference
```

**Supported model types**: `qwen`, `codellama`, `llama`, `mistral`

**Hardware requirements**:
- 4-16GB RAM depending on model size
- Optional GPU acceleration (Metal on macOS, CUDA on Linux)

## Agent Configuration

```toml
[agent]
# Context and token settings
fallback_default_max_tokens = 8192   # Default max tokens if provider doesn't specify
# max_context_length = 200000        # Override context window size for all providers

# Behavior settings
enable_streaming = true              # Stream responses in real-time
allow_multiple_tool_calls = true     # Allow multiple tools per response
timeout_seconds = 60                 # Request timeout
auto_compact = true                  # Auto-compact context at 90%

# Retry settings
max_retry_attempts = 3               # Retries for interactive mode
autonomous_max_retry_attempts = 6    # Retries for autonomous mode

# TODO management
check_todo_staleness = true          # Warn about stale TODO items
```

### Retry Behavior

g3 automatically retries on recoverable errors:
- Rate limits (HTTP 429)
- Network errors
- Server errors (HTTP 5xx)
- Timeouts

**Interactive mode** uses `max_retry_attempts` (default: 3)  
**Autonomous mode** uses `autonomous_max_retry_attempts` (default: 6) with longer delays

## Computer Control Configuration

```toml
[computer_control]
enabled = false              # Set to true to enable
require_confirmation = true  # Require confirmation before actions
max_actions_per_second = 5   # Rate limit for safety
```

**Required OS permissions**:
- **macOS**: System Preferences → Security & Privacy → Accessibility
- **Linux**: X11 or Wayland access
- **Windows**: Run as administrator (first time)

## WebDriver Configuration

```toml
[webdriver]
enabled = false              # Set to true to enable
browser = "safari"           # "safari" or "chrome-headless"
safari_port = 4444           # Safari WebDriver port
chrome_port = 9515           # ChromeDriver port
# chrome_binary = "/path/to/chrome"  # Optional: Custom Chrome path
```

### Safari Setup (macOS)

```bash
# Enable Safari remote automation (one-time setup)
safaridriver --enable

# Or via Safari UI:
# Safari → Preferences → Advanced → Show Develop menu
# Develop → Allow Remote Automation
```

### Chrome Setup

**Option 1: Chrome for Testing (Recommended)**
```bash
./scripts/setup-chrome-for-testing.sh
```

Then configure:
```toml
[webdriver]
chrome_binary = "/Users/yourname/.chrome-for-testing/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
```

**Option 2: System Chrome**
```bash
# macOS
brew install chromedriver

# Linux
apt install chromium-chromedriver
```

## macOS Accessibility API Configuration

```toml
enabled = false              # Set to true to enable
```

**Required permissions**: System Preferences → Security & Privacy → Privacy → Accessibility → Add your terminal app


## Multi-Role Configuration

For autonomous mode with different models for coach and player:

```toml
[providers]
default_provider = "anthropic.default"
coach = "anthropic.coach"    # Code reviewer
player = "anthropic.player"  # Code implementer

[providers.anthropic.coach]
api_key = "sk-ant-..."
model = "claude-sonnet-4-5"
max_tokens = 32000
temperature = 0.1            # Lower for consistent reviews

[providers.anthropic.player]
api_key = "sk-ant-..."
model = "claude-sonnet-4-5"
max_tokens = 64000
temperature = 0.3            # Higher for creative implementations
```

See `config.coach-player.example.toml` for a complete example.

## Environment Variables

Environment variables override configuration file settings:

| Variable | Description |
|----------|-------------|
| `G3_WORKSPACE_PATH` | Override workspace directory |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `DATABRICKS_HOST` | Databricks workspace URL |
| `DATABRICKS_TOKEN` | Databricks personal access token |

## CLI Overrides

CLI arguments have the highest priority:

```bash
# Override provider
g3 --provider anthropic.default

# Override model
g3 --model claude-opus-4-5

# Enable features
g3 --webdriver           # Enable WebDriver (Safari)
g3 --chrome-headless     # Enable WebDriver (Chrome headless)

# Specify config file
g3 --config /path/to/config.toml
```

## Complete Example Configuration

```toml
# ~/.config/g3/config.toml

[providers]
default_provider = "anthropic.default"

[providers.anthropic.default]
api_key = "sk-ant-api03-..."
model = "claude-sonnet-4-5"
max_tokens = 64000
temperature = 0.3

[providers.databricks.work]
host = "https://mycompany.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
max_tokens = 4096
temperature = 0.1
use_oauth = true

[agent]
fallback_default_max_tokens = 8192
enable_streaming = true
allow_multiple_tool_calls = true
timeout_seconds = 60
max_retry_attempts = 3
autonomous_max_retry_attempts = 6

[computer_control]
enabled = false
require_confirmation = true
max_actions_per_second = 5

[webdriver]
enabled = true
browser = "safari"
safari_port = 4444

enabled = false
```

## Troubleshooting

### "Old config format" error

If you see this error, your config uses a deprecated format. Update to the new named provider format:

**Old format** (deprecated):
```toml
[providers.anthropic]
api_key = "..."
```

**New format**:
```toml
[providers.anthropic.default]
api_key = "..."
```

### Provider not found

Ensure your `default_provider` matches a configured provider:
```toml
default_provider = "anthropic.default"  # Must match [providers.anthropic.default]
```

### OAuth issues

For Databricks OAuth:
1. Ensure `use_oauth = true`
2. Remove any `token` setting
3. A browser window will open for authentication
4. Tokens are cached in `~/.databricks/oauth-tokens.json`

### Context window errors

If you see context overflow errors:
1. Check `max_context_length` in `[agent]`
2. Use `/compact` command to manually compact
3. Use `/thinnify` to replace large tool results with file references
