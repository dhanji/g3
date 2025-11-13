//! G3 Ensembles - Multi-agent ensemble functionality
//!
//! This crate provides functionality for running multiple G3 agents in coordination,
//! enabling parallel development across different architectural modules.

pub mod flock;
pub mod status;
mod tests;

/// Re-export main types for convenience
pub use flock::{FlockConfig, FlockMode};
pub use status::{FlockStatus, SegmentStatus};
