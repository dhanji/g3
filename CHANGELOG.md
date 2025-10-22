# Changelog

## [Unreleased]

### Added

**Interactive Requirements Mode**
- **AI-Enhanced Interactive Requirements**: New `--interactive-requirements` flag for autonomous mode
  - User enters brief description of what they want to build
  - AI automatically enhances input into structured requirements.md document
  - Generates professional markdown with:
    - Project title and overview
    - Organized requirements (functional, technical, quality)
    - Acceptance criteria
  - User can review, accept, edit manually, or cancel before proceeding
  - Seamlessly transitions to autonomous mode

**Autonomous Mode Configuration**
- **Autonomous Mode Configuration**: Added ability to specify different models for coach and player agents in autonomous mode
  - New `[autonomous]` configuration section in `g3.toml`
  - `coach_provider` and `coach_model` options for coach agent
  - `player_provider` and `player_model` options for player agent
  - `Config::for_coach()` and `Config::for_player()` methods to generate role-specific configurations
  - Comprehensive test suite for autonomous configuration

### Changed
- Autonomous mode now uses `config.for_player()` for the player agent
- Coach agent creation now uses `config.for_coach()` for the coach agent

### Benefits
- **Cost Optimization**: Use cheaper models for execution, expensive models for review
- **Speed Optimization**: Use faster models for iteration, thorough models for validation
- **Specialization**: Leverage different providers' strengths for different roles
