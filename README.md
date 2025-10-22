# G3 - AI Coding Agent

G3 is a coding AI agent designed to help you complete tasks by writing code and executing commands. Built in Rust, it provides a flexible architecture for interacting with various Large Language Model (LLM) providers while offering powerful code generation and task automation capabilities.

## Key Features

- **Multiple LLM Providers**: Anthropic (Claude), Databricks, OpenAI, and local models via llama.cpp
- **Autonomous Mode**: Coach-player feedback loop for complex tasks
- **Intelligent Context Management**: Auto-summarization and context thinning at 50-80% thresholds
- **Rich Tool Ecosystem**: File operations, shell commands, computer control, browser automation
- **Streaming Responses**: Real-time output with tool call detection
- **Error Recovery**: Automatic retry logic with exponential backoff

## Getting Started

```bash
# Build the project
cargo build --release

# Execute a single task
g3 "implement a function to calculate fibonacci numbers"

# Start autonomous mode with interactive requirements
g3 --autonomous --interactive-requirements
```

## Configuration

Create `~/.config/g3/config.toml`:

```toml
[providers]
default_provider = "databricks"

[providers.anthropic]
api_key = "sk-ant-..."
model = "claude-3-5-sonnet-20241022"
max_tokens = 4096

[providers.databricks]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-meta-llama-3-1-70b-instruct"
max_tokens = 4096
use_oauth = true

[agent]
max_context_length = 8192
enable_streaming = true

# Optional: Use different models for coach and player in autonomous mode
[autonomous]
coach_provider = "anthropic"
coach_model = "claude-3-5-sonnet-20241022"  # Thorough review
player_provider = "databricks"
player_model = "databricks-meta-llama-3-1-70b-instruct"  # Fast execution
```

## Autonomous Mode (Coach-Player Loop)

G3 features an autonomous mode where two agents collaborate:
- **Player Agent**: Executes tasks and implements solutions
- **Coach Agent**: Reviews work and provides feedback

### Option 1: Interactive Requirements with AI Enhancement (Recommended)

```bash
g3 --autonomous --interactive-requirements
```

**How it works:**
1. Describe what you want to build (can be brief)
2. Press **Ctrl+D** (Unix/Mac) or **Ctrl+Z** (Windows)
3. AI enhances your input into a structured requirements document
4. Review the enhanced requirements
5. Choose to proceed, edit manually, or cancel
6. If accepted, autonomous mode starts automatically

**Example:**
```
You type: "build a todo app with cli in python"

AI generates:
# Todo List CLI Application

## Overview
A command-line todo list application built in Python...

## Functional Requirements
1. Add tasks with descriptions
2. Mark tasks as complete
3. Delete tasks
...
```

### Option 2: Direct Requirements

```bash
g3 --autonomous --requirements "Build a REST API with CRUD operations for user management"
```

### Option 3: Requirements File

Create `requirements.md` in your workspace:

```markdown
# Project Requirements

1. Create a REST API with user endpoints
2. Use SQLite for storage
3. Include input validation
4. Write unit tests
```

Then run:

```bash
g3 --autonomous
```

### Why Different Models for Coach and Player?

Configure different models in the `[autonomous]` section to:
- **Optimize Cost**: Use cheaper model for execution, expensive for review
- **Optimize Speed**: Use fast model for iteration, thorough for validation
- **Specialize**: Leverage provider strengths (e.g., Claude for analysis, Llama for code)

If not configured, both agents use the `default_provider` and its model.

## Command-Line Options

```bash
# Autonomous mode
g3 --autonomous --interactive-requirements
g3 --autonomous --requirements "Your requirements"
g3 --autonomous --max-turns 10

# Single-shot mode
g3 "your task here"

# Options
--workspace <DIR>          # Set workspace directory
--provider <NAME>          # Override provider (anthropic, databricks, openai)
--model <NAME>             # Override model
--quiet                    # Disable log files
--webdriver                # Enable browser automation
--show-prompt              # Show system prompt
--show-code                # Show generated code
```

## Architecture Overview

G3 is organized as a Rust workspace with multiple crates:

- **g3-core**: Agent engine, context management, tool system, streaming parser
- **g3-providers**: LLM provider abstraction (Anthropic, Databricks, OpenAI, local models)
- **g3-config**: Configuration management
- **g3-execution**: Task execution framework
- **g3-computer-control**: Mouse/keyboard automation, OCR, screenshots
- **g3-cli**: Command-line interface

### Key Capabilities

**Intelligent Context Management**
- Automatic context window monitoring with percentage-based tracking
- Smart auto-summarization when approaching token limits
- Context thinning at 50%, 60%, 70%, 80% thresholds
- Dynamic token allocation (4k to 200k+ tokens)

**Tool Ecosystem**
- File operations (read, write, edit with line-range precision)
- Shell command execution
- TODO management
- Computer control (experimental): mouse, keyboard, OCR, screenshots
- Browser automation via WebDriver (Safari)

**Error Handling**
- Automatic retry logic with exponential backoff
- Recoverable error detection (rate limits, network issues, timeouts)
- Detailed error logging to `logs/errors/`

## WebDriver Browser Automation

**One-Time Setup** (macOS):

```bash
# Enable Safari Remote Automation
safaridriver --enable  # Requires password

# Or via Safari UI:
# Safari → Preferences → Advanced → Show Develop menu
# Then: Develop → Allow Remote Automation
```

**Usage**:

```bash
g3 --webdriver "scrape the top stories from Hacker News"
```

See [docs/webdriver-setup.md](docs/webdriver-setup.md) for detailed setup.

## Computer Control (Experimental)

Enable in config:

```toml
[computer_control]
enabled = true
require_confirmation = true
```

Grant accessibility permissions:
- **macOS**: System Preferences → Security & Privacy → Accessibility
- **Linux**: Ensure X11 or Wayland access
- **Windows**: Run as administrator (first time)

**Available Tools**: `mouse_click`, `type_text`, `find_element`, `take_screenshot`, `extract_text`, `find_text_on_screen`, `list_windows`

## Use Cases

- Automated code generation and refactoring
- File manipulation and project scaffolding
- System administration tasks
- Data processing and transformation
- API integration and testing
- Documentation generation
- Complex multi-step workflows
- Desktop application automation

## Session Logs

G3 automatically saves session logs to `logs/` directory:
- Complete conversation history
- Token usage statistics
- Timestamps and session status

Disable with `--quiet` flag.

## Technology Stack

- **Language**: Rust (2021 edition)
- **Async Runtime**: Tokio
- **HTTP Client**: Reqwest
- **Serialization**: Serde
- **CLI Framework**: Clap
- **Logging**: Tracing
- **Local Models**: llama.cpp with Metal acceleration

## License

MIT License - see LICENSE file for details

## Contributing

Contributions welcome! Please see CONTRIBUTING.md for guidelines.
