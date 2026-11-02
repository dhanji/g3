//! Tool execution module for G3 agent.
//!
//! This module contains all tool implementations that the agent can execute.
//! Tools are organized by category:
//! - `shell` - Shell command execution and background processes
//! - `file_ops` - File reading, writing, and editing
//! - `plan` - Plan Mode for structured task planning
//! - `webdriver` - Browser automation via WebDriver
//! - `misc` - Other tools (screenshots, code search, etc.)
//! - `memory` - Workspace memory (remember)

pub mod executor;
pub mod datalog;
pub mod envelope;
pub mod file_ops;
pub mod invariants;
pub mod memory;
pub mod misc;
pub mod plan;
pub mod research;
pub mod shell;
pub mod toolsets;
pub mod webdriver;

pub use executor::ToolExecutor;
