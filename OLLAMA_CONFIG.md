# Configuring Ollama Provider in G3

This guide shows you how to configure G3 to use Ollama as your LLM provider.

## Quick Start

### 1. Install Ollama

```bash
# Visit https://ollama.ai to download and install
# Or use curl:
curl https://ollama.ai/install.sh | sh
```

### 2. Pull a Model

```bash
ollama pull llama3.2
# or any other model you prefer
```

### 3. Create Configuration File

Copy the example configuration:

```bash
cp config.ollama.example.toml ~/.config/g3/config.toml
```

Or create it manually:

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "llama3.2"
```

### 4. Run G3

```bash
g3
# G3 will now use Ollama with llama3.2!
```

## Configuration Options

### Basic Configuration

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "llama3.2"
```

This is the minimal configuration needed. It uses all defaults:
- Base URL: `http://localhost:11434`
- Temperature: `0.7`
- Max tokens: Not limited (uses model default)

### Full Configuration

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "llama3.2"
base_url = "http://localhost:11434"
max_tokens = 2048
temperature = 0.7
```

### Custom Ollama Host

If you're running Ollama on a different machine or port:

```toml
[providers.ollama]
model = "llama3.2"
base_url = "http://192.168.1.100:11434"
```

### Different Models

You can use any Ollama model:

```toml
[providers.ollama]
model = "qwen2.5:7b"  # Alibaba's Qwen model
```

```toml
[providers.ollama]
model = "mistral"  # Mistral AI
```

```toml
[providers.ollama]
model = "llama3.1:70b"  # Larger Llama model
```

## Multiple Provider Configuration

You can configure multiple providers and switch between them:

```toml
[providers]
default_provider = "ollama"  # Default for most operations

# Ollama for local, fast responses
[providers.ollama]
model = "llama3.2:3b"
temperature = 0.7

# Databricks for more complex tasks
[providers.databricks]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
max_tokens = 4096
temperature = 0.1
use_oauth = true
```

Then switch providers with:

```bash
g3 --provider databricks
```

## Autonomous Mode (Coach-Player)

Use different providers for code review (coach) and implementation (player):

```toml
[providers]
default_provider = "ollama"
coach = "databricks"  # Use powerful cloud model for review
player = "ollama"     # Use local model for implementation

[providers.ollama]
model = "qwen2.5:14b"  # Larger local model for coding

[providers.databricks]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
use_oauth = true
```

This gives you the best of both worlds:
- Fast local execution for coding tasks
- Powerful cloud review for quality assurance

## Recommended Models

### For Coding Tasks

| Model | Size | Speed | Quality | Notes |
|-------|------|-------|---------|-------|
| **qwen2.5:7b** | 7B | Fast | Excellent | Best balance for coding |
| **llama3.2:3b** | 3B | Very Fast | Good | Great for quick tasks |
| **llama3.1:8b** | 8B | Medium | Very Good | Solid all-rounder |
| **mistral** | 7B | Fast | Good | Good for general use |

### For Complex Tasks

| Model | Size | Speed | Quality | Notes |
|-------|------|-------|---------|-------|
| **qwen2.5:14b** | 14B | Medium | Excellent | Best local model for coding |
| **qwen2.5:32b** | 32B | Slow | Outstanding | If you have the resources |
| **llama3.1:70b** | 70B | Very Slow | Outstanding | Requires significant RAM/GPU |

## Temperature Settings

Temperature controls randomness in responses:

- **0.1-0.3**: Deterministic, good for code generation
- **0.5-0.7**: Balanced, good for most tasks
- **0.8-1.0**: Creative, good for brainstorming

```toml
[providers.ollama]
model = "qwen2.5:7b"
temperature = 0.2  # Focused code generation
```

## Max Tokens

Control response length:

```toml
[providers.ollama]
model = "llama3.2"
max_tokens = 1024  # Shorter responses
```

```toml
[providers.ollama]
model = "qwen2.5:7b"
max_tokens = 4096  # Longer, detailed responses
```

Leave it unset for model defaults (recommended).

## Performance Tuning

### GPU Acceleration

Ollama automatically uses GPU if available. To check:

```bash
ollama ps
```

### Quantized Models

For faster responses with less RAM:

```toml
[providers.ollama]
model = "llama3.2:3b-q4_0"  # 4-bit quantization
```

Quantization options:
- `q4_0`: 4-bit, fastest, lowest quality
- `q5_0`: 5-bit, balanced
- `q8_0`: 8-bit, slower, better quality

### Multiple Models

You can pull multiple models and switch easily:

```bash
ollama pull llama3.2:3b    # Fast for chat
ollama pull qwen2.5:7b     # Better for code
ollama pull mistral        # General purpose
```

Then change your config:

```toml
[providers.ollama]
model = "qwen2.5:7b"  # Just change this line
```

## Troubleshooting

### Ollama Not Running

```bash
# Check if Ollama is running
curl http://localhost:11434/api/version

# Start Ollama (macOS/Linux)
ollama serve

# Or just run a model (auto-starts)
ollama run llama3.2
```

### Model Not Found

```bash
# List available models
ollama list

# Pull the model
ollama pull llama3.2
```

### Slow Responses

1. Use a smaller model:
   ```toml
   model = "llama3.2:1b"  # Smallest, fastest
   ```

2. Use quantized version:
   ```toml
   model = "llama3.2:3b-q4_0"
   ```

3. Reduce max_tokens:
   ```toml
   max_tokens = 512
   ```

### Out of Memory

1. Switch to smaller model
2. Use quantized version
3. Close other applications
4. Check GPU memory: `ollama ps`

### Connection Refused

Check base_url is correct:

```toml
[providers.ollama]
model = "llama3.2"
base_url = "http://localhost:11434"  # Default
```

For remote Ollama:

```toml
base_url = "http://your-server:11434"
```

## Complete Example Configs

### Minimal Local Setup

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "llama3.2"

[agent]
max_context_length = 8192
enable_streaming = true
timeout_seconds = 60
```

### Optimized for Coding

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "qwen2.5:7b"
temperature = 0.2
max_tokens = 2048

[agent]
max_context_length = 16384
enable_streaming = true
timeout_seconds = 120
```

### Fast Responses

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "llama3.2:3b-q4_0"
temperature = 0.7
max_tokens = 1024

[agent]
max_context_length = 4096
enable_streaming = true
timeout_seconds = 30
```

### High Quality (Requires Good Hardware)

```toml
[providers]
default_provider = "ollama"

[providers.ollama]
model = "qwen2.5:32b"
temperature = 0.3
max_tokens = 4096

[agent]
max_context_length = 32768
enable_streaming = true
timeout_seconds = 300
```

### Hybrid (Local + Cloud)

```toml
[providers]
default_provider = "ollama"
coach = "databricks"
player = "ollama"

[providers.ollama]
model = "qwen2.5:14b"
temperature = 0.2

[providers.databricks]
host = "https://your-workspace.cloud.databricks.com"
model = "databricks-claude-sonnet-4"
use_oauth = true

[agent]
max_context_length = 16384
enable_streaming = true
timeout_seconds = 120
```

## Environment Variables

You can override config with environment variables:

```bash
# Override model
G3_PROVIDERS_OLLAMA_MODEL=qwen2.5:7b g3

# Override base URL
G3_PROVIDERS_OLLAMA_BASE_URL=http://192.168.1.100:11434 g3

# Override default provider
G3_PROVIDERS_DEFAULT_PROVIDER=ollama g3
```

## Best Practices

1. **Start Small**: Begin with llama3.2:3b, scale up if needed
2. **Use Quantization**: q4_0 or q5_0 for best speed/quality balance
3. **Match Task to Model**: 
   - Quick edits: 1B-3B models
   - Code generation: 7B-14B models
   - Complex refactoring: 14B-32B models
4. **Temperature for Code**: Use 0.1-0.3 for deterministic output
5. **Enable Streaming**: Always enable for better UX
6. **Local First**: Use Ollama by default, cloud for special cases

## Comparison with Other Providers

| Feature | Ollama | Databricks | OpenAI | Anthropic |
|---------|--------|------------|--------|-----------|
| Cost | Free | Paid | Paid | Paid |
| Privacy | Full | Medium | Low | Low |
| Speed (small models) | Fast | Fast | Medium | Medium |
| Speed (large models) | Slow | Fast | Fast | Fast |
| Setup Complexity | Low | Medium | Low | Low |
| Authentication | None | OAuth/Token | API Key | API Key |
| Offline Support | Yes | No | No | No |
| Tool Calling | Yes | Yes | Yes | Yes |

## Next Steps

1. Try different models: `ollama pull mistral`, `ollama pull qwen2.5`
2. Experiment with temperature settings
3. Set up hybrid config with cloud provider for complex tasks
4. Share your config in the community!

## Getting Help

- Ollama docs: https://ollama.ai/docs
- G3 issues: https://github.com/your-repo/issues
- Test your config: `g3 --help`
