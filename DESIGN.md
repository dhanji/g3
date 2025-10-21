# G3 - AI Coding Agent - Design Document

## Overview

G3 is a **modular, composable AI coding agent** built in Rust that helps you complete tasks by writing and executing code. It provides a flexible architecture for interacting with various Large Language Model (LLM) providers while offering powerful code generation, file manipulation, and task automation capabilities.

The agent follows a **tool-first philosophy**: instead of just providing advice, G3 actively uses tools to read files, write code, execute commands, and complete tasks autonomously.

## Core Principles

1. **Tool-First Philosophy**: Solve problems by actively using tools rather than just providing advice
2. **Modular Architecture**: Clear separation of concerns across multiple Rust crates
3. **Provider Flexibility**: Support multiple LLM providers through a unified interface
4. **Modularity**: Clear separation of concerns
5. **Composability**: Components can be combined in different ways
6. **Performance**: Built in Rust for speed and reliability
7. **Context Intelligence**: Smart context window management with auto-summarization
8. **Error Resilience**: Robust error handling with automatic retry logic

## Project Structure

G3 is organized as a Rust workspace with the following crates:

```
g3/
├── src/main.rs                   # Main entry point (delegates to g3-cli)
├── crates/
│   ├── g3-cli/                   # Command-line interface, TUI, and retro mode
│   ├── g3-core/                  # Core agent engine, tools, and streaming logic
│   ├── g3-providers/             # LLM provider abstractions and implementations
│   ├── g3-config/                # Configuration management
│   ├── g3-execution/             # Code execution engine
│   └── g3-computer-control/      # Computer control and automation
├── logs/                         # Session logs (auto-created)
├── README.md                     # Project documentation
└── DESIGN.md                     # This design document
```

## Architecture Overview

### High-Level Architecture

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   g3-cli        │    │   g3-core       │    │ g3-providers    │
│                 │    │                 │    │                 │
│ • CLI parsing   │◄──►│ • Agent engine  │◄──►│ • Anthropic     │
│ • Interactive   │    │ • Context mgmt  │    │ • Databricks    │
│ • Retro TUI     │    │ • Tool system   │    │ • Embedded      │
│ • Autonomous    │    │ • Streaming     │    │   (llama.cpp)   │
│   mode          │    │ • Task exec     │    │ • OAuth flow    │
│                 │    │ • TODO mgmt     │    │                 │
└─────────────────┘    └─────────────────┘    └─────────────────┘
         │                       │                       │
         └───────────────────────┼───────────────────────┘
                                 │
                    ┌─────────────────┐    ┌─────────────────┐
                    │ g3-execution    │    │   g3-config     │
                    │                 │    │                 │
                    │ • Code exec     │    │ • TOML config   │
                    │ • Shell cmds    │    │ • Env overrides │
                    │ • Streaming     │    │ • Provider      │
                    │ • Error hdlg    │    │   settings      │
                    └─────────────────┘    │ • Computer      │
                             │              │   control cfg   │
                             │              └─────────────────┘
                             │                       │
                    ┌─────────────────┐             │
                    │ g3-computer-    │◄────────────┘
                    │   control       │
                    │ • Mouse/kbd     │
                    │ • Screenshots   │
                    │ • OCR/Tesseract │
                    │ • Windows/UI    │
                    └─────────────────┘
```

## Core Components

### 1. g3-core: Agent Engine

**Primary Responsibilities:**
- Main orchestration logic for handling conversations and task execution
- Context window management with intelligent token tracking
- Built-in tool system for file operations and command execution
- Streaming response parsing with real-time tool call detection
- Error handling with automatic retry logic

**Key Features:**
- **Context Window Intelligence**: Automatic monitoring with percentage-based tracking (80% capacity triggers auto-summarization)
- **Tool System**: Built-in tools for file operations (read, write, edit), shell commands, and structured output
- **Streaming Parser**: Real-time parsing of LLM responses with tool call detection and execution
- **Session Management**: Automatic session logging with detailed conversation history and token usage
- **Error Recovery**: Sophisticated error classification and retry logic for recoverable errors
- **TODO Management**: In-memory TODO list with read/write tools for task tracking

**Available Tools:**
- `shell`: Execute shell commands with streaming output
- `read_file`: Read file contents with optional character range support
- `write_file`: Create or overwrite files with content
- `str_replace`: Apply unified diffs to files with precise editing
- `final_output`: Signal task completion with detailed summaries
- `todo_read`: Read the entire TODO list content
- `todo_write`: Write or overwrite the entire TODO list
- `mouse_click`: Click the mouse at specific coordinates
- `type_text`: Type text at the current cursor position
- `find_element`: Find UI elements by text, role, or attributes
- `take_screenshot`: Capture screenshots of screen, region, or window
- `extract_text`: Extract text from images or screen regions using OCR
- `find_text_on_screen`: Find text visually on screen and return coordinates
- `list_windows`: List all open windows with IDs and titles

### 2. g3-providers: LLM Provider Abstraction

**Primary Responsibilities:**
- Unified interface for multiple LLM providers
- Provider-specific optimizations and feature support
- OAuth authentication flows
- Streaming and non-streaming completion support

**Supported Providers:**
- **Anthropic**: Claude models via API with native tool calling support
- **Databricks**: Foundation Model APIs with OAuth and token-based authentication (default provider)
- **Embedded**: Local models via llama.cpp with GPU acceleration (Metal/CUDA)
- **Provider Registry**: Dynamic provider management and hot-swapping

**Key Features:**
- **Native Tool Calling**: Full support for structured tool calls where available
- **Fallback Parsing**: JSON tool call parsing for providers without native support
- **OAuth Integration**: Built-in OAuth flow for secure provider authentication
- **Context-Aware**: Provider-specific context length and token limit handling
- **Streaming Support**: Real-time response streaming with tool call detection

### 3. g3-cli: Command-Line Interface

**Primary Responsibilities:**
- Command-line argument parsing and validation
- Interactive terminal interface with history support
- Retro-style terminal UI (80s sci-fi inspired)
- Autonomous mode with coach-player feedback loops
- Session management and workspace handling

**Execution Modes:**
- **Single-shot**: Execute one task and exit
- **Interactive**: REPL-style conversation with the agent (default mode)
- **Autonomous**: Coach-player feedback loop for complex projects
- **Retro TUI**: Full-screen terminal interface with real-time updates

**Key Features:**
- **Multi-line Input**: Support for complex, multi-line prompts with backslash continuation
- **Context Progress**: Real-time display of token usage and context window status
- **Error Recovery**: Automatic retry logic for timeout and recoverable errors
- **History Management**: Persistent command history across sessions
- **Theme Support**: Customizable color themes for retro mode
- **Cancellation**: Ctrl+C support for graceful operation cancellation

### 4. g3-execution: Code Execution Engine

**Primary Responsibilities:**
- Safe execution of shell commands and scripts
- Streaming output capture and display
- Multi-language code execution support
- Error handling and result formatting

**Supported Execution:**
- **Bash/Shell**: Direct command execution with streaming output (primary use case)
- **Python**: Script execution via temporary files (legacy support)
- **JavaScript**: Node.js-based execution (legacy support)

**Key Features:**
- **Streaming Output**: Real-time command output display
- **Error Capture**: Comprehensive stderr and stdout handling
- **Exit Code Tracking**: Proper success/failure detection
- **Async Execution**: Non-blocking command execution
- **Output Formatting**: Clean, user-friendly result presentation

### 5. g3-config: Configuration Management

**Primary Responsibilities:**
- TOML-based configuration file management
- Environment variable overrides
- Provider-specific settings and credentials
- CLI argument integration

**Configuration Hierarchy:**
1. Default configuration (Databricks provider with OAuth)
2. Configuration files (`~/.config/g3/config.toml`, `./g3.toml`)
3. Environment variables (`G3_*`)
4. CLI arguments (highest priority)

**Key Features:**
- **Auto-generation**: Creates default configuration files if none exist
- **Provider Overrides**: Runtime provider and model selection
- **Validation**: Configuration validation with helpful error messages
- **Flexible Paths**: Support for shell expansion (`~`, environment variables)

### 6. g3-computer-control: Computer Control & Automation

**Primary Responsibilities:**
- Cross-platform computer control and automation
- Mouse and keyboard input simulation
- Window management and screenshot capture
- OCR text extraction from images and screen regions

**Platform Support:**
- **macOS**: Core Graphics, Cocoa, screencapture integration
- **Linux**: X11/Xtest for input, X11 for window management
- **Windows**: Win32 APIs for input and window control

**Key Features:**
- **OCR Integration**: Tesseract-based text extraction from images
- **Window Management**: List, identify, and capture specific application windows
- **UI Automation**: Find elements, simulate clicks, type text
- **Screenshot Capture**: Full screen, regions, or specific windows
- **Accessibility**: Requires OS-level permissions for automation

## Advanced Features

### Context Window Management

G3 implements sophisticated context window management:

- **Automatic Monitoring**: Tracks token usage with percentage-based thresholds
- **Smart Summarization**: Auto-triggers at 80% capacity to prevent context overflow
- **Context Thinning**: Progressive thinning at 50%, 60%, 70%, 80% thresholds - replaces large tool results with file references
- **Conversation Preservation**: Maintains conversation continuity through intelligent summaries
- **Provider-Specific Limits**: Adapts to different model context windows (4k to 200k+ tokens)
- **Cumulative Tracking**: Monitors total token usage across entire sessions

### Error Handling & Resilience

Comprehensive error handling system:

- **Error Classification**: Distinguishes between recoverable and non-recoverable errors
- **Automatic Retry**: Exponential backoff with jitter for rate limits, timeouts, and server errors
- **Detailed Logging**: Comprehensive error context including stack traces and session data
- **Error Persistence**: Saves detailed error logs to `logs/errors/` for analysis
- **Graceful Degradation**: Continues operation when possible, fails gracefully when not

### Session Management

Automatic session tracking and logging:

- **Session IDs**: Generated based on initial prompts for easy identification
- **Complete Logs**: Full conversation history, token usage, and timing data
- **JSON Format**: Structured logs for easy parsing and analysis
- **Automatic Cleanup**: Organized in `logs/` directory with timestamps
- **Status Tracking**: Records session completion status (completed, cancelled, error)

### Autonomous Mode

Advanced autonomous operation with coach-player feedback:

- **Requirements-Driven**: Reads `requirements.md` for project specifications
- **Dual-Agent System**: Separate player (implementation) and coach (review) agents
- **Iterative Improvement**: Multiple rounds of implementation and feedback
- **Progress Tracking**: Detailed reporting of turns, token usage, and final status
- **Workspace Management**: Automatic workspace setup and file organization

## Provider Comparison

| Feature | Anthropic | Databricks (Default) | Embedded |
|---------|-----------|------------|----------|
| **Cost** | Pay per token | Pay per token | Free after download |
| **Privacy** | Data sent to API | Data sent to API | Completely local |
| **Performance** | Very fast | Very fast | Depends on hardware |
| **Model Quality** | Excellent | Excellent | Good (varies by model) |
| **Offline Support** | No | No | Yes |
| **Setup Complexity** | API key only | OAuth or token | Model download required |
| **Context Window** | 200k tokens | Varies by model | 4k-32k tokens |
| **Tool Calling** | Native support | Native support | JSON fallback |
| **Hardware Requirements** | None | None | 4-16GB RAM, optional GPU |

## Configuration Examples

### Cloud-First Setup (Anthropic)
```toml
[providers]
default_provider = "anthropic"

[providers.anthropic]
api_key = "sk-ant-..."
model = "claude-3-5-sonnet-20241022"
max_tokens = 8192
temperature = 0.1
```

### Enterprise Setup (Databricks - Default)
```toml
[providers]
default_provider = "databricks"

[providers.databricks]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
max_tokens = 32000
temperature = 0.1
use_oauth = true
```

### Privacy-First Setup (Local Models)
```toml
[providers]
default_provider = "embedded"

[providers.embedded]
model_path = "~/.cache/g3/models/qwen2.5-7b-instruct-q3_k_m.gguf"
model_type = "qwen"
context_length = 32768
max_tokens = 2048
temperature = 0.1
gpu_layers = 32
threads = 8
```

### Hybrid Setup
```toml
[providers]
default_provider = "embedded"

# Local model for most tasks
[providers.embedded]
model_path = "~/.cache/g3/models/codellama-7b-instruct.Q4_K_M.gguf"
model_type = "codellama"
context_length = 16384
gpu_layers = 32

# Cloud fallback for complex tasks
[providers.anthropic]
api_key = "sk-ant-..."
model = "claude-3-5-sonnet-20241022"
```

## Usage Examples

### Single-Shot Mode
```bash
g3 "implement a fibonacci function in Rust"
```

### Interactive Mode
```bash
g3
g3> read the README and suggest improvements
g3> implement the suggestions you made
```

### Autonomous Mode
```bash
g3 --autonomous --max-turns 10
# Reads requirements.md and implements iteratively
```

### Retro TUI Mode
```bash
g3 --retro --theme dracula
# Full-screen terminal interface
```

## Implementation Details

### Planned Features
- **Plugin System**: Custom tool and provider plugins
- **Web Interface**: Browser-based UI for remote access
- **Model Quantization**: Optimized local model deployment
- **Multi-Model Ensemble**: Combine multiple models for better results
- **Advanced Sandboxing**: Enhanced security for code execution
- **Collaborative Mode**: Multi-user sessions and shared workspaces

### Technical Improvements
- **Performance Optimization**: Faster streaming and tool execution
- **Memory Management**: Better handling of large contexts and files
- **Caching System**: Intelligent caching of model responses and computations
- **Monitoring**: Built-in metrics and performance monitoring
- **Testing**: Comprehensive test suite and CI/CD integration

## Development Guidelines

### Code Organization
- **Modular Design**: Each crate has a single, well-defined responsibility
- **Trait-Based**: Use traits for abstraction and testability
- **Error Handling**: Comprehensive error types with context
- **Documentation**: Inline docs and examples for all public APIs
- **Testing**: Unit tests, integration tests, and property-based testing

### Performance Considerations
- **Async-First**: All I/O operations are asynchronous (Tokio runtime)
- **Streaming**: Real-time response processing where possible
- **Memory Efficiency**: Careful memory management for large contexts
- **Caching**: Strategic caching of expensive operations
- **Profiling**: Regular performance profiling and optimization

This design document reflects the current state of G3 as a mature, production-ready AI coding agent with sophisticated architecture and comprehensive feature set.

## Current Implementation Status

### Fully Implemented
- ✅ **Core Agent Engine**: Complete with streaming, tool execution, and context management
- ✅ **Provider System**: Anthropic, Databricks, and Embedded providers with OAuth support
- ✅ **Tool System**: 13 tools including file ops, shell, TODO management, and computer control
- ✅ **CLI Interface**: Interactive mode, single-shot mode, retro TUI
- ✅ **Autonomous Mode**: Coach-player feedback loop with requirements.md processing
- ✅ **Configuration**: TOML-based config with environment overrides
- ✅ **Error Handling**: Comprehensive retry logic and error classification
- ✅ **Session Logging**: Automatic session tracking and JSON logs
- ✅ **Context Management**: Context thinning (50-80%) and auto-summarization at 80% capacity
- ✅ **Computer Control**: Cross-platform automation with OCR support
- ✅ **TODO Management**: In-memory TODO list with read/write tools

### Architecture Highlights
- **Workspace**: 6 crates with clear separation of concerns
- **Dependencies**: Modern Rust ecosystem (Tokio, Clap, Serde, etc.)
- **Streaming**: Real-time response processing with tool call detection
- **Cross-Platform**: Works on macOS, Linux, and Windows
- **GPU Support**: Metal acceleration for local models on macOS, CUDA on Linux
- **OCR Support**: Tesseract integration for text extraction from images

### Key Files
- `src/main.rs`: main entry point delegating to g3-cli
- `crates/g3-core/src/lib.rs`: main agent implementation
- `crates/g3-cli/src/lib.rs`: CLI and interaction modes
- `crates/g3-providers/src/lib.rs`: provider trait and registry
- `crates/g3-config/src/lib.rs`: configuration management
- `crates/g3-execution/src/lib.rs`: code execution engine
- `crates/g3-computer-control/src/lib.rs`: computer control and automation
- `crates/g3-computer-control/src/platform/`: platform-specific implementations
