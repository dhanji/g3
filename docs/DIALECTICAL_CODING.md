# Dialectical Coding: The Player-Coach Pattern

G3 implements a **dialectical coding pattern** where two distinct agents—**Player** and **Coach**—alternate turns to iteratively implement and review code until requirements are satisfied.

## How It Works

```
Turn 1:
  PLAYER → Implements requirements from scratch
  COACH  → Reviews implementation, provides feedback

Turn 2:
  PLAYER → Addresses coach feedback
  COACH  → Reviews improvements

... continues until IMPLEMENTATION_APPROVED or max turns reached
```

## The Player Phase

The player receives either initial requirements or coach feedback and implements accordingly:

```rust
// g3-cli/src/lib.rs:2049-2059
let player_prompt = if coach_feedback.is_empty() {
    format!(
        "You are G3 in implementation mode. Read and implement the following requirements:\n\n{}\n\n...",
        requirements, requirements_sha
    )
} else {
    format!(
        "You are G3 in implementation mode. Address the following specific feedback from the coach:\n\n{}\n\nContext: You are improving an implementation based on these requirements:\n{}...",
        coach_feedback, requirements
    )
};
```

A fresh player agent executes the task (`g3-cli/src/lib.rs:2091-2108`) with up to 3 retries on failure (`g3-cli/src/lib.rs:2087`).

## The Coach Phase

A **separate agent instance** with fresh context reviews the implementation:

```rust
// g3-cli/src/lib.rs:2244
let coach_config = base_config.for_coach()?;
// ...
let mut coach_agent = Agent::new_autonomous_with_readme_and_quiet(coach_config, ...).await?;
```

The coach prompt (`g3-cli/src/lib.rs:2266-2294`) instructs review against requirements:

```rust
// g3-cli/src/lib.rs:2286-2287
"If the implementation thoroughly meets all requirements, compiles and is fully tested...
- Call final_output with summary: 'IMPLEMENTATION_APPROVED'"
```

## Approval Detection

The loop terminates when coach approves (`g3-cli/src/lib.rs:2490`):

```rust
// g3-cli/src/lib.rs:2490
if coach_result.is_approved() || coach_feedback_text.contains("IMPLEMENTATION_APPROVED") {
    output.print("\n=== SESSION COMPLETED - IMPLEMENTATION APPROVED ===");
    implementation_approved = true;
    break;
}
```

The `is_approved()` method checks for the magic string (`g3-core/src/task_result.rs:81-85`):

```rust
pub fn is_approved(&self) -> bool {
    self.extract_final_output()
        .contains("IMPLEMENTATION_APPROVED")
}
```

## Feedback Extraction

Coach feedback is extracted through a robust multi-method pipeline (`g3-core/src/feedback_extraction.rs:79-135`):

| Priority | Source | Reliability |
|----------|--------|-------------|
| 1 | Session log file | Highest |
| 2 | Native tool call JSON | High |
| 3 | Conversation history | Medium |
| 4 | TaskResult parsing | Low |
| 5 | Default fallback | Last resort |

```rust
// g3-core/src/feedback_extraction.rs:95-99
pub fn extract_coach_feedback<W>(
    coach_result: &TaskResult,
    agent: &Agent<W>,
    config: &FeedbackExtractionConfig,
) -> ExtractedFeedback
```

## Separate Provider Configuration

Coach and player can use different models/temperatures for their distinct roles:

```rust
// g3-config/src/lib.rs:514-528
pub fn get_coach_provider(&self) -> &str {
    self.providers.coach.as_deref().unwrap_or(&self.providers.default_provider)
}

pub fn get_player_provider(&self) -> &str {
    self.providers.player.as_deref().unwrap_or(&self.providers.default_provider)
}
```

Example configuration:
```toml
[providers]
coach = "anthropic.coach"    # Lower temperature for careful review
player = "anthropic.player"  # Higher temperature for creative implementation
```

## Turn Limits and Metrics

The loop has a configurable maximum (`g3-cli/src/lib.rs:2498-2501`):

```rust
if turn >= max_turns {
    output.print("\n=== SESSION COMPLETED - MAX TURNS REACHED ===");
    break;
}
```

Each turn tracks tokens and wall-clock time (`g3-cli/src/lib.rs:2512-2516`) for performance analysis.

## Key Design Principles

1. **Separation of Concerns**: Player focuses on implementation; coach focuses on review
2. **Fresh Context**: Each coach gets a new agent instance to avoid context pollution
3. **Robust Feedback**: Multi-method extraction ensures feedback is never lost
4. **Graceful Degradation**: Retries, fallbacks, and max-turn limits prevent infinite loops
5. **Configurable Roles**: Different models/temperatures can optimize each role's behavior
