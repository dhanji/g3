# Changelog: Requirements Persistence Feature

## Summary

Enhanced the accumulative autonomous mode (`--auto` / default mode) to automatically persist requirements to a local `.g3/requirements.md` file.

## Changes Made

### 1. Core Implementation (`crates/g3-cli/src/lib.rs`)

#### New Functions Added:

- **`ensure_g3_dir(workspace_dir: &Path) -> Result<PathBuf>`**
  - Creates `.g3` directory in the workspace if it doesn't exist
  - Returns the path to the `.g3` directory

- **`load_existing_requirements(workspace_dir: &Path) -> Result<Vec<String>>`**
  - Loads requirements from `.g3/requirements.md` if the file exists
  - Parses numbered requirements (format: `1. requirement text`)
  - Returns empty vector if file doesn't exist

- **`save_requirements(workspace_dir: &Path, requirements: &[String]) -> Result<()>`**
  - Saves accumulated requirements to `.g3/requirements.md`
  - Creates `.g3` directory if needed
  - Formats as markdown with numbered list

#### Modified Functions:

- **`run_accumulative_mode()`**
  - Now loads existing requirements on startup
  - Displays loaded requirements to user
  - Initializes turn number based on existing requirements count
  - Saves requirements after each new requirement is added
  - Shows save confirmation message
  - Updated `/requirements` command to show file location

### 2. Version Control (`.gitignore`)

- Added `.g3/` directory to `.gitignore`
- Prevents accidental commit of local requirements
- Users can opt-in to version control if desired

### 3. Documentation

#### New Documentation:

- **`docs/REQUIREMENTS_PERSISTENCE.md`**
  - Comprehensive guide to the requirements persistence feature
  - Usage examples and commands
  - File format specification
  - Use cases and best practices
  - Comparison with traditional autonomous mode

#### Updated Documentation:

- **`README.md`**
  - Added requirements persistence section to "Getting Started"
  - Highlighted key benefits (resume, review, share)
  - Added example showing `.g3/requirements.md` usage

### 4. Testing

- **`test_requirements.sh`**
  - Simple test script for manual verification
  - Creates test directory and provides instructions

## User-Facing Changes

### New Behavior

1. **Automatic Saving**
   - Every requirement entered is immediately saved to `.g3/requirements.md`
   - User sees confirmation: `💾 Saved to .g3/requirements.md`

2. **Automatic Loading**
   - On startup, G3 checks for existing `.g3/requirements.md`
   - If found, loads and displays requirements
   - Shows: `📂 Loaded N existing requirement(s) from .g3/requirements.md`

3. **Enhanced `/requirements` Command**
   - Now shows file location in output
   - Format: `📋 Accumulated Requirements (saved to .g3/requirements.md):`

4. **Session Resumability**
   - Users can exit and resume work later
   - Requirements persist across sessions
   - Turn numbering continues from previous session

### File Structure

```
my-project/
├── .g3/
│   └── requirements.md    # NEW: Accumulated requirements
├── logs/                  # Existing: Session logs
└── ... (project files)
```

### Requirements File Format

```markdown
# Project Requirements

1. First requirement
2. Second requirement
3. Third requirement
```

## Benefits

1. **Persistence**: No data loss if G3 crashes or is interrupted
2. **Transparency**: Always know what G3 is working on
3. **Resumability**: Pick up where you left off
4. **Documentation**: Requirements serve as project documentation
5. **Collaboration**: Share requirements with team members
6. **Auditability**: Track what was requested and when

## Backward Compatibility

- ✅ Fully backward compatible
- ✅ No breaking changes to existing functionality
- ✅ Works seamlessly with existing projects
- ✅ Graceful handling of missing `.g3` directory
- ✅ Error handling for file I/O issues

## Error Handling

- If `.g3/requirements.md` cannot be read: Shows warning, continues with empty requirements
- If `.g3/requirements.md` cannot be written: Shows warning, continues with in-memory requirements
- Non-blocking errors don't interrupt workflow

## Testing Checklist

- [x] Build succeeds without errors
- [ ] Manual test: Create new requirements in fresh directory
- [ ] Manual test: Resume session with existing requirements
- [ ] Manual test: `/requirements` command shows file location
- [ ] Manual test: Requirements file format is correct
- [ ] Manual test: Error handling for permission issues
- [ ] Manual test: `.g3` directory is created automatically
- [ ] Manual test: `.g3` directory is ignored by git

## Future Enhancements

Potential improvements for future versions:

1. Requirement status tracking (pending, in-progress, completed)
2. Requirement dependencies and ordering
3. Requirement templates and snippets
4. Integration with issue trackers
5. Requirement validation and linting
6. Export to other formats (JSON, YAML, etc.)
7. Requirement search and filtering
8. Requirement history and versioning

## Migration Guide

No migration needed! The feature works automatically:

1. Update to the new version
2. Run `g3` in any directory
3. Enter requirements as usual
4. Requirements are automatically saved to `.g3/requirements.md`

## Related Files

- `crates/g3-cli/src/lib.rs` - Core implementation
- `.gitignore` - Version control exclusion
- `docs/REQUIREMENTS_PERSISTENCE.md` - Feature documentation
- `README.md` - Updated getting started guide
- `test_requirements.sh` - Test script
