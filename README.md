# G3 - AI Coding Agent

G3 is a coding AI agent designed to help you complete tasks by writing code and executing commands. Built in Rust, it provides a flexible architecture for interacting with various Large Language Model (LLM) providers while offering powerful code generation and task automation capabilities.

## Architecture Overview

G3 follows a modular architecture organized as a Rust workspace with multiple crates, each responsible for specific functionality:

### Core Components

#### **g3-core**
The heart of the agent system, containing:
- **Agent Engine**: Main orchestration logic for handling conversations, tool execution, and task management
- **Context Window Management**: Intelligent tracking of token usage with auto-summarization capabilities when approaching context limits (~80% capacity)
- **Tool System**: Built-in tools for file operations (read, write, edit), shell command execution, and structured output generation
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
- Screenshot capture
- OCR text extraction

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
- Conversation history preservation through summaries
- Dynamic token allocation for different providers

### Tool Ecosystem
- **File Operations**: Read, write, and edit files with line-range precision
- **Shell Integration**: Execute system commands with output capture
- **Code Generation**: Structured code generation with syntax awareness
- **Computer Control** (Experimental): Automate desktop applications
  - **OCR Support**: Extract and find text from images and screen regions using Tesseract
  - Mouse and keyboard control
  - UI element inspection
  - Screenshot capture
  - See [Computer Control Guide](docs/COMPUTER_CONTROL.md) for details
- **Final Output**: Formatted result presentation

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
- Desktop application automation and testing

## Getting Started

```bash
# Build the project
cargo build --release

# Run G3
cargo run

# Execute a task
g3 "implement a function to calculate fibonacci numbers"
```

## Computer Control (Experimental)

G3 can interact with your computer's GUI for automation tasks:

### Setup

1. Enable in config:
```toml
[computer_control]
enabled = true
```

2. Grant OS permissions:
   - **macOS**: System Preferences → Security & Privacy → Accessibility
   - **Linux**: Ensure X11 or Wayland access
   - **Windows**: Run as administrator (first time only)

3. Use computer control:
```bash
```

See [Computer Control Guide](docs/COMPUTER_CONTROL.md) for detailed documentation.

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
