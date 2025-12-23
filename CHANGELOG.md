# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - 2025-12-16

### Added
- **OpenRouter Support**: Implemented full support for OpenRouter as an LLM provider.
    - Added `OpenRouterProvider` implementation in `crates/g3-providers`.
    - Added configuration structures `OpenRouterConfig` and `ProviderPreferencesConfig` in `crates/g3-config`.
    - Added integration tests in `crates/tests/openrouter_integration_tests.rs`.
    - Updated `g3-cli` to accept `openrouter` as a valid provider type in command line arguments.
    - Updated `g3-core` to register and handle OpenRouter providers.
    - Added example configuration in `config.example.toml`.
- **Configuration**: Added `config.toml` to `.gitignore`.

### Changed
- **g3-core**: Updated `provider_max_tokens` and `resolve_max_tokens` to correctly handle OpenRouter configuration and context window sizes (defaulting to 128k if not specified, but respecting config).
- **g3-cli**: Updated provider validation logic to support `openrouter` prefix (e.g., `openrouter.grok`).

### Fixed
- **g3-providers**: Fixed unused variable warnings in `anthropic.rs` by renaming `cache_config` to `_cache_config`.
- **g3-planner**: Fixed unused function warning in `llm.rs` by renaming `print_status_line` to `_print_status_line`.
- **g3-providers**: Fixed typo in `openrouter.rs` (`pub mode` -> `pub mod`).
