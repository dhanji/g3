# Accumulative Autonomous Mode

## Overview

Accumulative Autonomous Mode is the **new default interactive mode** for G3. It combines the ease of interactive chat with the power of autonomous implementation, allowing you to build projects iteratively by describing what you want, one requirement at a time.

## How It Works

### The Flow

1. **Start G3** in any directory (no arguments needed)
2. **Describe** what you want to build
3. **G3 automatically**:
   - Adds your input to accumulated requirements
   - Runs autonomous mode (coach-player feedback loop)
   - Implements your requirements with quality checks
4. **Continue** adding more requirements or refinements
5. **Repeat** until your project is complete

### Example Session

```bash
$ cd ~/projects/my-new-app
$ g3

🪿 G3 AI Coding Agent - Accumulative Mode
      >> describe what you want, I'll build it iteratively

📁 Workspace: /Users/you/projects/my-new-app

💡 Each input you provide will be added to requirements
   and I'll automatically work on implementing them.

   Type 'exit' or 'quit' to stop, Ctrl+D to finish

============================================================
📝 What would you like me to build? (describe your requirements)
============================================================
requirement> create a simple web server in Python with Flask that serves a homepage

📋 Current instructions and requirements (Turn 1):
   create a simple web server in Python with Flask that serves a homepage

🚀 Starting autonomous implementation...

🤖 G3 AI Coding Agent - Autonomous Mode
📁 Using workspace: /Users/you/projects/my-new-app
📋 Requirements loaded from --requirements flag
🔄 Starting coach-player feedback loop...
📂 No existing implementation files detected
🎯 Starting with player implementation

=== TURN 1/5 - PLAYER MODE ===
🎯 Starting player implementation...
📋 Player starting initial implementation (no prior coach feedback)

[Player creates files, writes code...]

=== TURN 1/5 - COACH MODE ===
🎓 Starting coach review...
🎓 Coach review completed
Coach feedback:
The Flask server is implemented correctly with a homepage route. 
The code follows best practices and meets the requirements.
IMPLEMENTATION_APPROVED

=== SESSION COMPLETED - IMPLEMENTATION APPROVED ===
✅ Coach approved the implementation!

============================================================
📊 AUTONOMOUS MODE SESSION REPORT
============================================================
⏱️  Total Duration: 12.34s
🔄 Turns Taken: 1/5
📝 Final Status: ✅ APPROVED
...
============================================================

✅ Autonomous run completed

============================================================
📝 Turn 2 - What's next? (add more requirements or refinements)
============================================================
requirement> add a /api/users endpoint that returns a list of users as JSON

📋 Current instructions and requirements (Turn 2):
   add a /api/users endpoint that returns a list of users as JSON

🚀 Starting autonomous implementation...

[Autonomous mode runs again with BOTH requirements...]

============================================================
📝 Turn 3 - What's next? (add more requirements or refinements)
============================================================
requirement> exit

👋 Goodbye!
```

## Key Features

### 1. Requirement Accumulation

Each input you provide is:
- **Numbered sequentially** (1, 2, 3, ...)
- **Stored in memory** for the session
- **Included in all subsequent runs**

This means the agent always has the full context of what you've asked for.

### 2. Automatic Requirements Document

G3 automatically generates a structured requirements document:

```markdown
# Project Requirements

## Current Instructions and Requirements:

1. create a simple web server in Python with Flask that serves a homepage
2. add a /api/users endpoint that returns a list of users as JSON
3. add error handling for 404 and 500 errors

## Latest Requirement (Turn 3):

add error handling for 404 and 500 errors
```

This document is passed to autonomous mode, ensuring the agent:
- Knows all previous requirements
- Focuses on the latest addition
- Maintains consistency across iterations

### 3. Full Autonomous Quality

Each requirement triggers a complete autonomous run with:
- **Coach-Player Feedback Loop**: Quality assurance built-in
- **Multiple Turns**: Up to 5 iterations per requirement (configurable with `--max-turns`)
- **Compilation Checks**: Ensures code actually works
- **Testing**: Coach can run tests to verify functionality

### 4. Error Recovery

If an autonomous run fails:
- You're notified of the error
- You can provide additional requirements to fix issues
- The session continues (doesn't crash)

### 5. Workspace Management

- Uses **current directory** as workspace
- All files created in current directory
- No need to specify workspace path
- Works with existing projects or empty directories

## Command-Line Options

### Default (Accumulative Mode)

```bash
g3
```

Starts accumulative autonomous mode in the current directory.

### With Options

```bash
# Use a specific workspace
g3 --workspace ~/projects/my-app

# Limit autonomous turns per requirement
g3 --max-turns 3

# Enable macOS Accessibility tools
g3 --macax

# Enable WebDriver browser automation
g3 --webdriver

# Use a specific provider/model
g3 --provider anthropic --model claude-3-5-sonnet-20241022

# Show prompts and code during execution
g3 --show-prompt --show-code

# Disable log files
g3 --quiet
```

### Disable Accumulative Mode

To use the traditional chat mode (without automatic autonomous runs):

```bash
g3 --chat

# Alternative: legacy flag also works
g3 --accumulative
```

This gives you the old behavior where you chat with the agent without automatic autonomous runs.

## Use Cases

### 1. Rapid Prototyping

```bash
requirement> create a REST API for a todo app
requirement> add SQLite database storage
requirement> add authentication with JWT
requirement> add rate limiting
```

### 2. Iterative Refinement

```bash
requirement> create a data visualization dashboard
requirement> make the charts interactive
requirement> add dark mode support
requirement> optimize for mobile devices
```

### 3. Bug Fixing

```bash
requirement> fix the login form validation
requirement> handle edge case when username is empty
requirement> add better error messages
```

### 4. Feature Addition

```bash
requirement> add export to CSV functionality
requirement> add email notifications
requirement> add admin dashboard
```

## Tips and Best Practices

### 1. Start Simple

Begin with a basic requirement, let it be implemented, then add complexity:

```bash
✅ Good:
requirement> create a basic Flask web server
requirement> add a homepage with a form
requirement> add form validation

❌ Too Complex:
requirement> create a full-stack web app with authentication, database, API, and frontend
```

### 2. Be Specific

The more specific you are, the better the results:

```bash
✅ Good:
requirement> add a /api/users endpoint that returns JSON with id, name, and email fields

❌ Vague:
requirement> add users
```

### 3. One Thing at a Time

Focus each requirement on a single feature or fix:

```bash
✅ Good:
requirement> add error handling for database connections
requirement> add logging for all API requests

❌ Multiple Things:
requirement> add error handling and logging and monitoring and alerts
```

### 4. Review Between Turns

After each autonomous run completes:
- Check the generated files
- Test the functionality
- Decide what to add or fix next

### 5. Use Exit Commands

When done:
- Type `exit` or `quit`
- Press `Ctrl+D` (EOF)
- Press `Ctrl+C` to cancel current input

## Comparison with Other Modes

| Feature | Accumulative (Default) | Traditional Interactive | Autonomous | Single-Shot |
|---------|----------------------|------------------------|------------|-------------|
| **Command** | `g3` | `g3 --accumulative` | `g3 --autonomous` | `g3 "task"` |
| **Input Style** | Iterative prompts | Chat messages | requirements.md file | Command-line arg |
| **Auto-Autonomous** | ✅ Yes | ❌ No | ✅ Yes | ❌ No |
| **Coach-Player Loop** | ✅ Yes | ❌ No | ✅ Yes | ❌ No |
| **Accumulates Requirements** | ✅ Yes | ❌ No | ❌ No | ❌ No |
| **Multiple Iterations** | ✅ Yes | ✅ Yes | ✅ Yes | ❌ No |
| **Best For** | Iterative development | Quick questions | Pre-planned projects | One-off tasks |

## Technical Details

### Requirements Storage

- Stored in memory (not persisted to disk)
- Numbered sequentially starting from 1
- Formatted as markdown list
- Passed to autonomous mode as `--requirements` override

### History

- Saved to `~/.g3_accumulative_history`
- Separate from traditional interactive history
- Persists across sessions
- Uses rustyline for readline support

### Workspace

- Defaults to current directory
- Can be overridden with `--workspace`
- All files created in workspace
- Logs saved to `workspace/logs/`

### Autonomous Execution

- Full coach-player feedback loop
- Configurable max turns (default: 5)
- Respects all CLI flags (--macax, --webdriver, etc.)
- Error handling allows continuation

## Troubleshooting

### "No requirements provided"

This shouldn't happen in accumulative mode, but if it does:
- Check that you entered a requirement
- Ensure the requirement isn't empty
- Try restarting G3

### "Autonomous run failed"

If an autonomous run fails:
- Read the error message
- Provide a new requirement to fix the issue
- Or type `exit` and investigate manually

### "Context window full"

If you hit token limits:
- The agent will auto-summarize
- Or you can start a new session
- Consider using `--max-turns` to limit iterations

### "Coach never approves"

If the coach keeps rejecting:
- Check the coach feedback for specific issues
- Provide more specific requirements
- Consider increasing `--max-turns`

## Future Enhancements

Planned improvements:

1. **Persistence**: Save accumulated requirements to disk
2. **Editing**: Edit or remove previous requirements
3. **Branching**: Try different approaches
4. **Templates**: Pre-defined requirement sets
5. **Review**: Show all accumulated requirements
6. **Export**: Save to requirements.md
7. **Undo**: Remove last requirement
8. **Replay**: Re-run with same requirements

## Feedback

This is a new feature! Please provide feedback:
- What works well?
- What's confusing?
- What features would you like?
- Any bugs or issues?

Open an issue on GitHub or contribute improvements!
