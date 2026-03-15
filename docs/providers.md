# g3 LLM Providers Guide

**Last updated**: January 2025 (Gemini provider added)  
**Source of truth**: `crates/g3-providers/src/`

## Purpose

This document describes the LLM providers supported by g3, their capabilities, and how to choose between them.

## Provider Overview

| Provider | Type | Tool Calling | Cache Control | Context Window | Best For |
|----------|------|--------------|---------------|----------------|----------|
| **Anthropic** | Cloud | Native | Yes | 200k (1M optional) | General use, complex tasks |
| **Databricks** | Cloud | Native | Yes (Claude models) | Varies | Enterprise, existing Databricks users |
| **Gemini** | Cloud | Native | No | 1M-2M | Google ecosystem, large context |
| **OpenAI** | Cloud | Native | No | 128k | GPT model preference |
| **OpenAI-Compatible** | Cloud | Native | No | Varies | OpenRouter, Groq, Together, etc. |
| **Embedded** | Local | JSON fallback | No | 4k-32k | Privacy, offline, cost savings |

## Anthropic

**Location**: `crates/g3-providers/src/anthropic.rs`

### Features

- **Native tool calling**: Full support for structured tool calls
- **Prompt caching**: Reduce costs with ephemeral caching
- **Extended context**: Optional 1M token context (additional cost)
- **Extended thinking**: Budget tokens for complex reasoning
- **Streaming**: Real-time response streaming

### Configuration

```toml
[providers.anthropic.default]
api_key = "sk-ant-api03-..."     # Required
model = "claude-sonnet-4-5"      # Model name
max_tokens = 64000               # Max output tokens
temperature = 0.3                # 0.0-1.0
cache_config = "ephemeral"       # Optional: Enable caching
enable_1m_context = true          # Optional: 1M context
thinking_budget_tokens = 10000    # Optional: Extended thinking
```

### Available Models

| Model | Context | Best For |
|-------|---------|----------|
| `claude-sonnet-4-5` | 200k | Balanced performance/cost |
| `claude-opus-4-5` | 200k | Complex reasoning |
| `claude-3-5-sonnet-20241022` | 200k | Previous generation |
| `claude-3-opus-20240229` | 200k | Previous generation |

### Prompt Caching

Enable caching to reduce costs for repeated context:

```toml
cache_config = "ephemeral"  # Cache for session duration
```

Caching is applied to:
- System prompts
- README/AGENTS.md content
- Large tool results

### Extended Thinking

For complex tasks requiring step-by-step reasoning:

```toml
thinking_budget_tokens = 10000  # Tokens for internal reasoning
```

The model uses these tokens for planning before responding.

---

## Databricks

**Location**: `crates/g3-providers/src/databricks.rs`

### Features

- **Foundation Model APIs**: Access to various models
- **OAuth authentication**: Secure browser-based auth
- **Token authentication**: Personal access tokens
- **Enterprise integration**: Works with existing Databricks setup

### Configuration

```toml
[providers.databricks.default]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
max_tokens = 4096
temperature = 0.1
use_oauth = true              # Recommended
# token = "dapi..."           # Alternative: PAT
```

### Authentication

**OAuth (Recommended)**:
1. Set `use_oauth = true`
2. On first run, browser opens for authentication
3. Tokens are cached in `~/.databricks/oauth-tokens.json`
4. Tokens refresh automatically

**Personal Access Token**:
1. Generate token in Databricks workspace
2. Set `token = "dapi..."` and `use_oauth = false`

### Available Models

Models depend on your Databricks workspace configuration:
- `databricks-claude-sonnet-4` (Claude via Databricks)
- `databricks-meta-llama-3-1-70b-instruct`
- `databricks-dbrx-instruct`
- Custom fine-tuned models

---

## Gemini

**Location**: `crates/g3-providers/src/gemini.rs`

### Features

- **Native tool calling**: Full support for structured tool calls
- **Large context windows**: Up to 2M tokens on some models
- **Streaming**: Real-time response streaming
- **Google ecosystem**: Integrates with Google Cloud

### Configuration

```toml
[providers.gemini.default]
api_key = "your-google-api-key"  # Required
model = "gemini-2.0-flash"       # Model name
max_tokens = 8192                # Max output tokens
temperature = 0.7                # 0.0-1.0
```

### Available Models

| Model | Context | Notes |
|-------|---------|-------|
| `gemini-2.0-flash` | 1M | Fast, efficient |
| `gemini-1.5-pro` | 2M | Most capable |
| `gemini-1.5-flash` | 1M | Balanced speed/quality |

### Getting an API Key

1. Go to [Google AI Studio](https://aistudio.google.com/)
2. Create or select a project
3. Generate an API key
4. Add to your g3 configuration

### Notes

- Gemini models have very large context windows (1M-2M tokens)
- Good for tasks requiring extensive context
- Native tool calling works well for agentic workflows

---

## OpenAI

**Location**: `crates/g3-providers/src/openai.rs`

### Features

- **Native tool calling**: Full support
- **Custom endpoints**: Override base URL
- **Streaming**: Real-time responses

### Configuration

```toml
[providers.openai.default]
api_key = "sk-..."               # Required
model = "gpt-4-turbo"            # Model name
max_tokens = 4096
temperature = 0.1
# base_url = "https://api.openai.com/v1"  # Optional
# reasoning_exclude = true        # Optional: add reasoning.exclude=true
```

### Available Models

| Model | Context | Notes |
|-------|---------|-------|
| `gpt-4-turbo` | 128k | Latest GPT-4 |
| `gpt-4o` | 128k | Optimized GPT-4 |
| `gpt-4` | 8k | Original GPT-4 |
| `gpt-3.5-turbo` | 16k | Faster, cheaper |

---

## OpenAI-Compatible Providers

**Location**: `crates/g3-providers/src/openai.rs` (reuses OpenAI implementation)

For services that implement the OpenAI API format.

### Configuration

```toml
# OpenRouter
[providers.openai_compatible.openrouter]
api_key = "sk-or-..."
model = "anthropic/claude-3.5-sonnet"
base_url = "https://openrouter.ai/api/v1"
max_tokens = 4096
temperature = 0.1
reasoning_exclude = true         # Optional: hide reasoning tokens from content

# Groq
[providers.openai_compatible.groq]
api_key = "gsk_..."
model = "llama-3.3-70b-versatile"
base_url = "https://api.groq.com/openai/v1"
max_tokens = 4096
temperature = 0.1

# Together
[providers.openai_compatible.together]
api_key = "..."
model = "meta-llama/Llama-3-70b-chat-hf"
base_url = "https://api.together.xyz/v1"
max_tokens = 4096
temperature = 0.1
```

`reasoning_exclude` is optional and disabled by default. Enable it per provider when you want
reasoning-capable models to return concise `content` without reasoning text.

### Supported Services

- **OpenRouter**: Access to many models through one API
- **Groq**: Fast inference for Llama models
- **Together**: Open-source model hosting
- **Anyscale**: Scalable model serving
- **Local servers**: Ollama, vLLM, text-generation-inference

---

## Embedded (Local Models)

**Location**: `crates/g3-providers/src/embedded.rs`

### Features

- **Completely local**: No data leaves your machine
- **Offline capable**: Works without internet
- **GPU acceleration**: Metal (macOS), CUDA (Linux)
- **No API costs**: Free after model download

### Configuration

```toml
[providers.embedded.default]
model_path = "~/.cache/g3/models/qwen2.5-7b-instruct-q3_k_m.gguf"
model_type = "qwen"              # Model architecture
context_length = 32768           # Context window
max_tokens = 2048                # Max output
temperature = 0.1
gpu_layers = 32                  # GPU offload (0 = CPU only)
threads = 8                      # CPU threads
```

### Supported Model Types

| Type | Models | Notes |
|------|--------|-------|
| `qwen` | Qwen 2.5 series | Good coding ability |
| `codellama` | Code Llama | Specialized for code |
| `llama` | Llama 2/3 | General purpose |
| `mistral` | Mistral/Mixtral | Efficient |

### Model Download

Download GGUF models from Hugging Face:

```bash
mkdir -p ~/.cache/g3/models
cd ~/.cache/g3/models

# Example: Qwen 2.5 7B
wget https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf
```

### Hardware Requirements

| Model Size | RAM Required | GPU VRAM | Notes |
|------------|--------------|----------|-------|
| 7B Q4 | 6GB | 4GB | Good for most tasks |
| 7B Q8 | 10GB | 8GB | Better quality |
| 13B Q4 | 10GB | 8GB | More capable |
| 70B Q4 | 48GB | 40GB | Requires high-end hardware |

### GPU Acceleration

**macOS (Metal)**:
```toml
gpu_layers = 32  # Offload layers to GPU
```

**Linux (CUDA)**:
Requires CUDA toolkit installed.

**CPU Only**:
```toml
gpu_layers = 0
threads = 8  # Use more threads
```

### Tool Calling

Embedded models don't have native tool calling. g3 uses JSON fallback:
1. System prompt includes tool definitions as JSON
2. Model outputs tool calls as JSON in response
3. g3 parses JSON and executes tools

This works but is less reliable than native tool calling.

---

## Provider Selection Guide

### By Use Case

| Use Case | Recommended Provider |
|----------|---------------------|
| General coding tasks | Anthropic (Claude Sonnet) |
| Complex reasoning | Anthropic (Claude Opus) |
| Enterprise/compliance | Databricks |
| Cost-sensitive | Embedded or Groq |
| Privacy-critical | Embedded |
| Offline development | Embedded |
| Fast iteration | Groq (Llama) |
| Large context needs | Gemini (1M-2M tokens) |
| Model variety | OpenRouter |

### By Priority

**Quality first**: Anthropic Claude Opus/Sonnet
- Best reasoning and coding ability
- Native tool calling
- Prompt caching for efficiency

**Cost first**: Embedded or OpenAI-compatible
- Embedded: Free after download
- Groq: Very cheap, fast
- OpenRouter: Pay-per-use, many options

**Privacy first**: Embedded
- Data never leaves your machine
- No API calls
- Full control

**Speed first**: Groq or Embedded with GPU
- Groq: Extremely fast inference
- Embedded with Metal/CUDA: Low latency

---

## Provider Trait

All providers implement the `LLMProvider` trait:

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Generate a completion
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    
    /// Stream a completion
    async fn stream(&self, request: CompletionRequest) -> Result<CompletionStream>;
    
    /// Provider name (e.g., "anthropic.default")
    fn name(&self) -> &str;
    
    /// Model name (e.g., "claude-sonnet-4-5")
    fn model(&self) -> &str;
    
    /// Whether provider supports native tool calling
    fn has_native_tool_calling(&self) -> bool;
    
    /// Whether provider supports cache control
    fn supports_cache_control(&self) -> bool;
    
    /// Configured max tokens
    fn max_tokens(&self) -> u32;
    
    /// Configured temperature
    fn temperature(&self) -> f32;
}
```

---

## Adding a New Provider

1. Create `crates/g3-providers/src/newprovider.rs`
2. Implement `LLMProvider` trait
3. Add configuration struct to `crates/g3-config/src/lib.rs`
4. Register in `crates/g3-core/src/lib.rs` (`new_with_mode_and_readme`)
5. Export from `crates/g3-providers/src/lib.rs`
6. Update documentation

---

## Troubleshooting

### Authentication Errors

**Anthropic**: Verify API key starts with `sk-ant-`

**Databricks OAuth**: 
- Delete `~/.databricks/oauth-tokens.json` and re-authenticate
- Ensure workspace URL is correct

**OpenAI**: Verify API key and check billing status

### Rate Limits

g3 automatically retries on rate limits with exponential backoff.

To reduce rate limit issues:
- Use prompt caching (Anthropic)
- Reduce `max_tokens`
- Use a provider with higher limits

### Context Window Errors

If you see "context too long" errors:
1. Use `/compact` to compact conversation
2. Use `/thinnify` to replace large tool results
3. Increase `max_context_length` in config
4. Switch to a provider with larger context

### Embedded Model Issues

**Model not loading**:
- Verify `model_path` is correct
- Check file permissions
- Ensure enough RAM

**Slow inference**:
- Increase `gpu_layers` for GPU offload
- Reduce `context_length`
- Use a smaller quantization (Q4 vs Q8)

**Poor tool calling**:
- Embedded models use JSON fallback
- Consider cloud provider for complex tool use
