# Ollama Provider for g3

A simple, local LLM provider implementation for g3 that connects to Ollama.

## Features

- ✅ **Simple Setup**: No API keys or authentication required
- ✅ **Local Execution**: Runs entirely on your machine
- ✅ **Tool Calling Support**: Native tool calling for compatible models
- ✅ **Streaming**: Full streaming support with real-time responses
- ✅ **Flexible Configuration**: Custom base URL, temperature, and max tokens
- ✅ **Model Discovery**: Automatic detection of available models

## Quick Start

### Prerequisites

1. Install and start Ollama: https://ollama.ai
2. Pull a model: `ollama pull llama3.2`

### Basic Usage

```rust
use g3_providers::{OllamaProvider, LLMProvider, CompletionRequest, Message, MessageRole};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create provider with default settings (localhost:11434)
    let provider = OllamaProvider::new(
        "llama3.2".to_string(),
        None,  // base_url: defaults to http://localhost:11434
        None,  // max_tokens: optional
        None,  // temperature: defaults to 0.7
    )?;

    // Create a simple request
    let request = CompletionRequest {
        messages: vec![
            Message {
                role: MessageRole::User,
                content: "What is the capital of France?".to_string(),
            },
        ],
        max_tokens: Some(1000),
        temperature: Some(0.7),
        stream: false,
        tools: None,
    };

    // Get completion
    let response = provider.complete(request).await?;
    println!("Response: {}", response.content);
    println!("Tokens: {}", response.usage.total_tokens);

    Ok(())
}
```

### Streaming Example

```rust
use futures_util::StreamExt;

let request = CompletionRequest {
    messages: vec![
        Message {
            role: MessageRole::User,
            content: "Write a short poem about coding".to_string(),
        },
    ],
    max_tokens: Some(500),
    temperature: Some(0.8),
    stream: true,
    tools: None,
};

let mut stream = provider.stream(request).await?;

while let Some(chunk_result) = stream.next().await {
    match chunk_result {
        Ok(chunk) => {
            print!("{}", chunk.content);
            if chunk.finished {
                println!("\n\nDone!");
                if let Some(usage) = chunk.usage {
                    println!("Total tokens: {}", usage.total_tokens);
                }
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
```

### Tool Calling Example

```rust
use serde_json::json;

let tools = vec![Tool {
    name: "get_weather".to_string(),
    description: "Get current weather for a location".to_string(),
    input_schema: json!({
        "type": "object",
        "properties": {
            "location": {
                "type": "string",
                "description": "City name"
            },
            "unit": {
                "type": "string",
                "enum": ["celsius", "fahrenheit"],
                "description": "Temperature unit"
            }
        },
        "required": ["location"]
    }),
}];

let request = CompletionRequest {
    messages: vec![
        Message {
            role: MessageRole::User,
            content: "What's the weather in Paris?".to_string(),
        },
    ],
    max_tokens: Some(500),
    temperature: Some(0.5),
    stream: false,
    tools: Some(tools),
};

let response = provider.complete(request).await?;
println!("Response: {}", response.content);
```

### Custom Ollama Host

```rust
// Connect to remote Ollama instance
let provider = OllamaProvider::new(
    "llama3.2".to_string(),
    Some("http://192.168.1.100:11434".to_string()),
    None,
    None,
)?;
```

### Fetch Available Models

```rust
// Discover what models are available
let models = provider.fetch_available_models().await?;
println!("Available models:");
for model in models {
    println!("  - {}", model);
}
```

## Supported Models

The provider works with any Ollama model, including:

- **llama3.2** (1B, 3B) - Meta's latest Llama models
- **llama3.1** (8B, 70B, 405B) - Previous generation
- **qwen2.5** (7B, 14B, 32B) - Alibaba's Qwen models  
- **mistral** - Mistral AI models
- **mixtral** - Mixture of experts model
- **phi3** - Microsoft's Phi-3
- **gemma2** - Google's Gemma 2

## Configuration

### Constructor Parameters

```rust
OllamaProvider::new(
    model: String,           // Model name (e.g., "llama3.2")
    base_url: Option<String>, // Ollama API URL (default: http://localhost:11434)
    max_tokens: Option<u32>,  // Maximum tokens to generate (optional)
    temperature: Option<f32>, // Sampling temperature (default: 0.7)
)
```

### Request Options

```rust
CompletionRequest {
    messages: Vec<Message>,      // Conversation history
    max_tokens: Option<u32>,     // Override provider's max_tokens
    temperature: Option<f32>,    // Override provider's temperature
    stream: bool,                // Enable streaming responses
    tools: Option<Vec<Tool>>,    // Tools for function calling
}
```

## Comparison with Other Providers

| Feature | Ollama | OpenAI | Anthropic | Databricks |
|---------|--------|--------|-----------|------------|
| Local Execution | ✅ | ❌ | ❌ | ❌ |
| Authentication | None | API Key | API Key | OAuth/Token |
| Tool Calling | ✅ | ✅ | ✅ | ✅ |
| Streaming | ✅ | ✅ | ✅ | ✅ |
| Cost | Free | Paid | Paid | Paid |
| Privacy | High | Low | Low | Medium |

## Implementation Details

### API Endpoints

- **Chat Completion**: `POST /api/chat`
- **Model List**: `GET /api/tags`

### Response Format

Ollama uses a simple JSON-per-line streaming format:

```json
{"message":{"role":"assistant","content":"Hello"},"done":false}
{"message":{"role":"assistant","content":" there"},"done":false}
{"done":true,"prompt_eval_count":10,"eval_count":20}
```

### Tool Call Format

Tool calls are returned in the message structure:

```json
{
  "message": {
    "role": "assistant",
    "content": "",
    "tool_calls": [
      {
        "function": {
          "name": "get_weather",
          "arguments": {"location": "Paris", "unit": "celsius"}
        }
      }
    ]
  },
  "done": true
}
```

## Troubleshooting

### Connection Errors

If you see connection errors, ensure Ollama is running:

```bash
# Check if Ollama is running
curl http://localhost:11434/api/version

# Start Ollama (if needed)
ollama serve
```

### Model Not Found

Pull the model first:

```bash
ollama pull llama3.2
ollama list  # Check available models
```

### Performance Issues

- Use smaller models (1B, 3B) for faster responses
- Reduce `max_tokens` to limit generation length
- Enable GPU acceleration if available
- Consider quantized models (e.g., `llama3.2:3b-q4_0`)

## Testing

Run the included tests:

```bash
cargo test --package g3-providers ollama
```

All tests should pass:
```
running 4 tests
test ollama::tests::test_custom_base_url ... ok
test ollama::tests::test_message_conversion ... ok
test ollama::tests::test_provider_creation ... ok
test ollama::tests::test_tool_conversion ... ok
```

## Architecture

The provider follows the same architecture as other g3 providers:

1. **OllamaProvider**: Main struct implementing `LLMProvider` trait
2. **Request/Response Structures**: Internal types for Ollama API
3. **Streaming Parser**: Handles line-by-line JSON parsing
4. **Tool Call Handling**: Accumulates and converts tool calls
5. **Error Handling**: Robust error handling with retries

## Contributing

The provider is part of the g3-providers crate. To contribute:

1. Add features to `ollama.rs`
2. Update tests
3. Run `cargo test --package g3-providers`
4. Update this documentation

## License

Same as the g3 project.
