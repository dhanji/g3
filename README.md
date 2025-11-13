# G3 - AI Coding Agent

G3 is a coding AI agent designed to help you complete tasks by writing code and executing commands. Built in Rust, it provides a flexible architecture for interacting with various Large Language Model (LLM) providers while offering powerful code generation and task automation capabilities.

## Architecture Overview

G3 follows a modular architecture organized as a Rust workspace with multiple crates, each responsible for specific functionality:

### Core Components

#### **g3-core**
The heart of the agent system, containing:
- **Agent Engine**: Main orchestration logic for handling conversations, tool execution, and task management
- **Context Window Management**: Intelligent tracking of token usage with context thinning (50-80%) and auto-summarization at 80% capacity
- **Tool System**: Built-in tools for file operations, shell commands, computer control, TODO management, and structured output
- **Streaming Response Parser**: Real-time parsing of LLM responses with tool call detection and execution
- **Task Execution**: Support for single and iterative task execution with automatic retry logic

#### **g3-providers**
Abstraction layer for LLM providers:
- **Provider Interface**: Common trait-based API for different LLM backends
- **Multiple Provider Support**: 
  - Anthropic (Claude models)
  - Databricks (DBRX and other models)
  - Local/embedded models via llama.cpp with Metal acceleration on macOS
- **OAuth Authentication**: Built-in OAuth flow support for secure provider authentication
- **Provider Registry**: Dynamic provider management and selection

#### **g3-config**
Configuration management system:
- Environment-based configuration
- Provider credentials and settings
- Model selection and parameters
- Runtime configuration options

#### **g3-execution**
Task execution framework:
- Task planning and decomposition
- Execution strategies (sequential, parallel)
- Error handling and retry mechanisms
- Progress tracking and reporting

#### **g3-computer-control**
Computer control capabilities:
- Mouse and keyboard automation
- UI element inspection and interaction
- Screenshot capture and window management
- OCR text extraction via Tesseract

#### **g3-cli**
Command-line interface:
- Interactive terminal interface
- Task submission and monitoring
- Configuration management commands
- Session management

### Error Handling & Resilience

G3 includes robust error handling with automatic retry logic:
- **Recoverable Error Detection**: Automatically identifies recoverable errors (rate limits, network issues, server errors, timeouts)
- **Exponential Backoff with Jitter**: Implements intelligent retry delays to avoid overwhelming services
- **Detailed Error Logging**: Captures comprehensive error context including stack traces, request/response data, and session information
- **Error Persistence**: Saves detailed error logs to `logs/errors/` for post-mortem analysis
- **Graceful Degradation**: Non-recoverable errors are logged with full context before terminating

## Key Features

### Intelligent Context Management
- Automatic context window monitoring with percentage-based tracking
- Smart auto-summarization when approaching token limits
- **Context thinning** at 50%, 60%, 70%, 80% thresholds - automatically replaces large tool results with file references
- Conversation history preservation through summaries
- Dynamic token allocation for different providers (4k to 200k+ tokens)

### Interactive Control Commands
G3's interactive CLI includes control commands for manual context management:
- **`/compact`**: Manually trigger summarization to compact conversation history
- **`/thinnify`**: Manually trigger context thinning to replace large tool results with file references
- **`/readme`**: Reload README.md and AGENTS.md from disk without restarting
- **`/stats`**: Show detailed context and performance statistics
- **`/help`**: Display all available control commands

These commands give you fine-grained control over context management, allowing you to proactively optimize token usage and refresh project documentation. See [Control Commands Documentation](docs/CONTROL_COMMANDS.md) for detailed usage.

### Tool Ecosystem
- **File Operations**: Read, write, and edit files with line-range precision
- **Shell Integration**: Execute system commands with output capture
- **Code Generation**: Structured code generation with syntax awareness
- **TODO Management**: Read and write TODO lists with markdown checkbox format
- **Computer Control** (Experimental): Automate desktop applications
  - Mouse and keyboard control
  - macOS Accessibility API for native app automation (via `--macax` flag)
  - UI element inspection
  - Screenshot capture and window management
  - OCR text extraction from images and screen regions
  - Window listing and identification
- **Code Search**: Embedded tree-sitter for syntax-aware code search (Rust, Python, JavaScript, TypeScript, Go, Java, C, C++) - see [Code Search Guide](docs/CODE_SEARCH.md)
- **Final Output**: Formatted result presentation
- **Flock Mode**: Parallel multi-agent development for large projects - see [Flock Mode Guide](docs/FLOCK_MODE.md)

### Provider Flexibility
- Support for multiple LLM providers through a unified interface
- Hot-swappable providers without code changes
- Provider-specific optimizations and feature support
- Local model support for offline operation

### Task Automation
- Single-shot task execution for quick operations
- Iterative task mode for complex, multi-step workflows
- Automatic error recovery and retry logic
- Progress tracking and intermediate result handling

## Language & Technology Stack

- **Language**: Rust (2021 edition)
- **Async Runtime**: Tokio for concurrent operations
- **HTTP Client**: Reqwest for API communications
- **Serialization**: Serde for JSON handling
- **CLI Framework**: Clap for command-line parsing
- **Logging**: Tracing for structured logging
- **Local Models**: llama.cpp with Metal acceleration support

## Use Cases

G3 is designed for:
- Automated code generation and refactoring
- File manipulation and project scaffolding
- System administration tasks
- Data processing and transformation  
- API integration and testing
- Documentation generation
- Complex multi-step workflows
- Parallel development of modular architectures
- Desktop application automation and testing

## Getting Started

### Default Mode: Accumulative Autonomous

The default interactive mode now uses **accumulative autonomous mode**, which combines the best of interactive and autonomous workflows:

```bash
# Simply run g3 in any directory
g3

# You'll be prompted to describe what you want to build
# Each input you provide:
# 1. Gets added to accumulated requirements
# 2. Automatically triggers autonomous mode (coach-player loop)
# 3. Implements your requirements iteratively

# Example session:
requirement> create a simple web server in Python with Flask
# ... autonomous mode runs and implements it ...
requirement> add a /health endpoint that returns JSON
# ... autonomous mode runs again with both requirements ...
```

### Other Modes

```bash
# Single-shot mode (one task, then exit)
g3 "implement a function to calculate fibonacci numbers"

# Traditional autonomous mode (reads requirements.md)
g3 --autonomous

# Traditional chat mode (simple interactive chat without autonomous runs)
g3 --chat
```

```bash
# Build the project
cargo build --release

# Run from the build directory
./target/release/g3

# Or copy both files to somewhere in your PATH (macOS only needs both files)
cp target/release/g3 ~/.local/bin/
cp target/release/libVisionBridge.dylib ~/.local/bin/  # macOS only

# Execute a task
g3 "implement a function to calculate fibonacci numbers"
```

## Configuration

G3 uses a TOML configuration file for settings. The config file is automatically created at `~/.config/g3/config.toml` on first run with sensible defaults.

### Retry Configuration

G3 includes configurable retry logic for handling recoverable errors (timeouts, rate limits, network issues, server errors):

```toml
[agent]
max_context_length = 8192
enable_streaming = true
timeout_seconds = 60

# Retry configuration for recoverable errors
max_retry_attempts = 3              # Default mode retry attempts
autonomous_max_retry_attempts = 6   # Autonomous mode retry attempts
```

**Retry Behavior:**
- **Default Mode** (`max_retry_attempts`): Used for interactive chat and single-shot tasks. Default: 3 attempts.
- **Autonomous Mode** (`autonomous_max_retry_attempts`): Used for long-running autonomous tasks. Default: 6 attempts.
- Retries use exponential backoff with jitter to avoid overwhelming services
- Autonomous mode spreads retries over ~10 minutes to handle extended outages
- Only recoverable errors are retried (timeouts, rate limits, 5xx errors, network issues)
- Non-recoverable errors (auth failures, invalid requests) fail immediately

**Example:** To increase timeout resilience in autonomous mode, set `autonomous_max_retry_attempts = 10` in your config.

See `config.example.toml` for a complete configuration example.

## WebDriver Browser Automation

G3 includes WebDriver support for browser automation tasks using Safari.

**One-Time Setup** (macOS only):

Safari Remote Automation must be enabled before using WebDriver tools. Run this once:

```bash
# Option 1: Use the provided script
./scripts/enable-safari-automation.sh

# Option 2: Enable manually
safaridriver --enable  # Requires password

# Option 3: Enable via Safari UI
# Safari → Preferences → Advanced → Show Develop menu
# Then: Develop → Allow Remote Automation
```

**For detailed setup instructions and troubleshooting**, see [WebDriver Setup Guide](docs/webdriver-setup.md).

**Usage**: Run G3 with the `--webdriver` flag to enable browser automation tools.

## macOS Accessibility API Tools

G3 includes support for controlling macOS applications via the Accessibility API, allowing you to automate native macOS apps.

**Available Tools**: `macax_list_apps`, `macax_get_frontmost_app`, `macax_activate_app`, `macax_get_ui_tree`, `macax_find_elements`, `macax_click`, `macax_set_value`, `macax_get_value`, `macax_press_key`

**Setup**: Enable with the `--macax` flag or in config with `macax.enabled = true`. Grant accessibility permissions:
- **macOS**: System Preferences → Security & Privacy → Privacy → Accessibility → Add your terminal app

**For detailed documentation**, see [macOS Accessibility Tools Guide](docs/macax-tools.md).

**Note**: This is particularly useful for testing and automating apps you're building with G3, as you can add accessibility identifiers to your UI elements.

## Computer Control (Experimental)

G3 can interact with your computer's GUI for automation tasks:

**Available Tools**: `mouse_click`, `type_text`, `find_element`, `take_screenshot`, `extract_text`, `find_text_on_screen`, `list_windows`

**Setup**: Enable in config with `computer_control.enabled = true` and grant OS accessibility permissions:
- **macOS**: System Preferences → Security & Privacy → Accessibility  
- **Linux**: Ensure X11 or Wayland access
- **Windows**: Run as administrator (first time only)

## Session Logs

G3 automatically saves session logs for each interaction in the `logs/` directory. These logs contain:
- Complete conversation history
- Token usage statistics
- Timestamps and session status

The `logs/` directory is created automatically on first use and is excluded from version control.

## License

MIT License - see LICENSE file for details

## Contributing

G3 is an open-source project. Contributions are welcome! Please see CONTRIBUTING.md for guidelines.
