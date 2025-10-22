# Coach-Player Provider Configuration

G3 now supports specifying different LLM providers for the coach and player agents when running in autonomous mode. This allows you to optimize for different requirements:

- **Player**: The agent that implements code - might benefit from a faster, more cost-effective model
- **Coach**: The agent that reviews code - might benefit from a more powerful, analytical model

## Configuration

In your `config.toml` file, under the `[providers]` section, you can specify:

```toml
[providers]
default_provider = "databricks"  # Used for normal operations
coach = "databricks"              # Provider for coach (code reviewer)
player = "anthropic"              # Provider for player (code implementer)
```

If `coach` or `player` are not specified, they will default to using the `default_provider`.

## Example Use Cases

### Cost Optimization
Use a cheaper, faster model for initial implementations (player) and a more powerful model for review (coach):

```toml
coach = "anthropic"  # Claude Sonnet for thorough review
player = "anthropic" # Claude Haiku for quick implementation
```

### Speed vs Quality Trade-off
Use a local embedded model for fast iterations (player) and a cloud model for quality review (coach):

```toml
coach = "databricks"  # Cloud model for quality review
player = "embedded"   # Local model for fast implementation
```

### Specialized Models
Use different models optimized for different tasks:

```toml
coach = "databricks"  # Model fine-tuned for code review
player = "openai"     # Model optimized for code generation
```

## Requirements

- Both providers must be properly configured in your config file
- Each provider must have valid credentials
- The models specified for each provider must be accessible

## How It Works

When running in autonomous mode (`g3 --autonomous`), the system will:

1. Use the `player` provider (or default) for the initial implementation
2. Switch to the `coach` provider (or default) for code review
3. Return to the `player` provider for implementing feedback
4. Continue this cycle for the specified number of turns

The providers are logged at startup so you can verify which models are being used:

```
🎮 Player provider: anthropic
👨‍🏫 Coach provider: databricks
ℹ️  Using different providers for player and coach
```

## Benefits

- **Cost Efficiency**: Use expensive models only where they add the most value
- **Speed Optimization**: Use faster models for iterative development
- **Specialization**: Leverage models that excel at specific tasks
- **Flexibility**: Easy to experiment with different provider combinations
