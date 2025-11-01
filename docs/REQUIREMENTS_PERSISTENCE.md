# Requirements Persistence in Accumulative Mode

## Overview

In accumulative autonomous mode (`--auto` or default mode), G3 now automatically persists your requirements to a local `.g3/requirements.md` file. This provides several benefits:

1. **Persistence across sessions**: Your requirements are saved and can be resumed later
2. **Version control friendly**: Requirements are stored in a readable markdown format
3. **Easy review**: You can view and edit requirements directly in the file
4. **Transparency**: Always know what G3 is working on

## How It Works

### Automatic Saving

When you run G3 in accumulative mode:

```bash
g3
```

Each requirement you enter is automatically:
1. Added to the accumulated requirements list
2. Saved to `.g3/requirements.md` in your workspace
3. Used for the autonomous implementation run

### File Format

The `.g3/requirements.md` file uses a simple numbered list format:

```markdown
# Project Requirements

1. Create a simple web server in Python with Flask
2. Add a /health endpoint that returns JSON
3. Add logging for all requests
```

### Loading Existing Requirements

When you start G3 in a directory that already has a `.g3/requirements.md` file, it will:

1. Automatically load the existing requirements
2. Display them on startup
3. Continue numbering from where you left off

Example output:

```
📂 Loaded 3 existing requirement(s) from .g3/requirements.md

   1. Create a simple web server in Python with Flask
   2. Add a /health endpoint that returns JSON
   3. Add logging for all requests

============================================================
📝 Turn 4 - What's next? (add more requirements or refinements)
============================================================
requirement> 
```

## Commands

### View Requirements

Use the `/requirements` command to view all accumulated requirements:

```
requirement> /requirements

📋 Accumulated Requirements (saved to .g3/requirements.md):

   1. Create a simple web server in Python with Flask
   2. Add a /health endpoint that returns JSON
   3. Add logging for all requests
```

### Other Commands

- `/help` - Show all available commands
- `/chat` - Switch to interactive chat mode (preserves requirements context)
- `exit` or `quit` - Exit the session

## File Location

The requirements file is stored at:

```
<workspace>/.g3/requirements.md
```

Where `<workspace>` is your current working directory.

## Version Control

The `.g3/` directory is automatically added to `.gitignore`, so your requirements won't be committed to version control by default. If you want to track requirements in git, you can:

1. Remove `.g3/` from `.gitignore`
2. Commit the `.g3/requirements.md` file

This can be useful for:
- Sharing requirements with team members
- Tracking requirement evolution over time
- Documenting project goals

## Manual Editing

You can manually edit `.g3/requirements.md` if needed. G3 will parse the file and load any numbered requirements (format: `1. requirement text`).

**Note**: Make sure to maintain the numbered list format for proper parsing.

## Error Handling

If G3 cannot save or load requirements, it will:

1. Display a warning message
2. Continue operating with in-memory requirements
3. Not interrupt your workflow

Example:

```
⚠️  Warning: Could not save requirements to .g3/requirements.md: Permission denied
```

## Use Cases

### Resuming Work

```bash
# Day 1: Start a project
cd my-project
g3
requirement> Create a REST API with user authentication
# ... work happens ...
exit

# Day 2: Resume work
cd my-project
g3
# G3 automatically loads previous requirements
requirement> Add password reset functionality
```

### Reviewing Progress

```bash
# Check what you've asked G3 to build
cat .g3/requirements.md

# Or use the command within G3
requirement> /requirements
```

### Sharing Requirements

```bash
# Share requirements with a team member
cp .g3/requirements.md requirements-backup.md
# Or commit to version control
git add .g3/requirements.md
git commit -m "Add project requirements"
```

## Implementation Details

### Functions

- `ensure_g3_dir()` - Creates `.g3` directory if it doesn't exist
- `load_existing_requirements()` - Loads requirements from `.g3/requirements.md`
- `save_requirements()` - Saves requirements to `.g3/requirements.md`

### File Structure

```
my-project/
├── .g3/
│   └── requirements.md    # Accumulated requirements
├── logs/                  # Session logs (existing)
└── ... (your project files)
```

## Benefits

1. **No data loss**: Requirements are persisted even if G3 crashes or is interrupted
2. **Transparency**: Always know what G3 is working on
3. **Resumability**: Pick up where you left off in any session
4. **Documentation**: Requirements serve as project documentation
5. **Collaboration**: Share requirements with team members
6. **Auditability**: Track what was requested and when

## Comparison with Traditional Autonomous Mode

| Feature | Accumulative Mode | Traditional `--autonomous` |
|---------|------------------|---------------------------|
| Requirements file | `.g3/requirements.md` | `requirements.md` (root) |
| Auto-save | ✅ Yes | ❌ No (manual edit) |
| Interactive | ✅ Yes | ❌ No |
| Incremental | ✅ Yes | ❌ No (one-shot) |
| Resume support | ✅ Yes | ⚠️ Manual |

## Future Enhancements

Potential future improvements:

- Requirement status tracking (pending, in-progress, completed)
- Requirement dependencies and ordering
- Requirement templates and snippets
- Integration with issue trackers
- Requirement validation and linting
