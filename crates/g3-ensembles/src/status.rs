//! Status tracking for flock mode

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Status of an individual segment worker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentStatus {
    /// Segment number
    pub segment_id: usize,
    
    /// Segment workspace directory
    pub workspace: PathBuf,
    
    /// Current state of the segment
    pub state: SegmentState,
    
    /// Start time
    pub started_at: DateTime<Utc>,
    
    /// Completion time (if finished)
    pub completed_at: Option<DateTime<Utc>>,
    
    /// Total tokens used
    pub tokens_used: u64,
    
    /// Number of tool calls made
    pub tool_calls: u64,
    
    /// Number of errors encountered
    pub errors: u64,
    
    /// Current turn number (for autonomous mode)
    pub current_turn: usize,
    
    /// Maximum turns allowed
    pub max_turns: usize,
    
    /// Last status message
    pub last_message: Option<String>,
    
    /// Error message (if failed)
    pub error_message: Option<String>,
}

/// State of a segment worker
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SegmentState {
    /// Waiting to start
    Pending,
    
    /// Currently running
    Running,
    
    /// Completed successfully
    Completed,
    
    /// Failed with error
    Failed,
    
    /// Cancelled by user
    Cancelled,
}

impl std::fmt::Display for SegmentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SegmentState::Pending => write!(f, "⏳ Pending"),
            SegmentState::Running => write!(f, "🔄 Running"),
            SegmentState::Completed => write!(f, "✅ Completed"),
            SegmentState::Failed => write!(f, "❌ Failed"),
            SegmentState::Cancelled => write!(f, "⚠️  Cancelled"),
        }
    }
}

/// Overall flock status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlockStatus {
    /// Flock session ID
    pub session_id: String,
    
    /// Project directory
    pub project_dir: PathBuf,
    
    /// Flock workspace directory
    pub flock_workspace: PathBuf,
    
    /// Number of segments
    pub num_segments: usize,
    
    /// Start time
    pub started_at: DateTime<Utc>,
    
    /// Completion time (if finished)
    pub completed_at: Option<DateTime<Utc>>,
    
    /// Status of each segment
    pub segments: HashMap<usize, SegmentStatus>,
    
    /// Total tokens used across all segments
    pub total_tokens: u64,
    
    /// Total tool calls across all segments
    pub total_tool_calls: u64,
    
    /// Total errors across all segments
    pub total_errors: u64,
}

impl FlockStatus {
    /// Create a new flock status
    pub fn new(
        session_id: String,
        project_dir: PathBuf,
        flock_workspace: PathBuf,
        num_segments: usize,
    ) -> Self {
        Self {
            session_id,
            project_dir,
            flock_workspace,
            num_segments,
            started_at: Utc::now(),
            completed_at: None,
            segments: HashMap::new(),
            total_tokens: 0,
            total_tool_calls: 0,
            total_errors: 0,
        }
    }
    
    /// Update segment status
    pub fn update_segment(&mut self, segment_id: usize, status: SegmentStatus) {
        self.segments.insert(segment_id, status);
        self.recalculate_totals();
    }
    
    /// Recalculate total metrics
    fn recalculate_totals(&mut self) {
        self.total_tokens = self.segments.values().map(|s| s.tokens_used).sum();
        self.total_tool_calls = self.segments.values().map(|s| s.tool_calls).sum();
        self.total_errors = self.segments.values().map(|s| s.errors).sum();
    }
    
    /// Check if all segments are complete
    pub fn is_complete(&self) -> bool {
        self.segments.len() == self.num_segments
            && self.segments.values().all(|s| {
                matches!(
                    s.state,
                    SegmentState::Completed | SegmentState::Failed | SegmentState::Cancelled
                )
            })
    }
    
    /// Get count of segments by state
    pub fn count_by_state(&self, state: SegmentState) -> usize {
        self.segments.values().filter(|s| s.state == state).count()
    }
    
    /// Save status to file
    pub fn save_to_file(&self, path: &PathBuf) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
    
    /// Load status from file
    pub fn load_from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let status = serde_json::from_str(&json)?;
        Ok(status)
    }
    
    /// Generate a summary report
    pub fn generate_report(&self) -> String {
        let mut report = String::new();
        
        report.push_str(&format!("\n{}", "=".repeat(80)));
        report.push_str(&format!("\n📊 FLOCK MODE SESSION REPORT"));
        report.push_str(&format!("\n{}", "=".repeat(80)));
        
        report.push_str(&format!("\n\n🆔 Session ID: {}", self.session_id));
        report.push_str(&format!("\n📁 Project: {}", self.project_dir.display()));
        report.push_str(&format!("\n🗂️  Workspace: {}", self.flock_workspace.display()));
        report.push_str(&format!("\n🔢 Segments: {}", self.num_segments));
        
        let duration = if let Some(completed) = self.completed_at {
            completed.signed_duration_since(self.started_at)
        } else {
            Utc::now().signed_duration_since(self.started_at)
        };
        
        report.push_str(&format!("\n⏱️  Duration: {:.2}s", duration.num_milliseconds() as f64 / 1000.0));
        
        // Segment status summary
        report.push_str(&format!("\n\n📈 Segment Status:"));
        report.push_str(&format!("\n   • Completed: {}", self.count_by_state(SegmentState::Completed)));
        report.push_str(&format!("\n   • Running: {}", self.count_by_state(SegmentState::Running)));
        report.push_str(&format!("\n   • Failed: {}", self.count_by_state(SegmentState::Failed)));
        report.push_str(&format!("\n   • Pending: {}", self.count_by_state(SegmentState::Pending)));
        report.push_str(&format!("\n   • Cancelled: {}", self.count_by_state(SegmentState::Cancelled)));
        
        // Metrics
        report.push_str(&format!("\n\n📊 Aggregate Metrics:"));
        report.push_str(&format!("\n   • Total Tokens: {}", self.total_tokens));
        report.push_str(&format!("\n   • Total Tool Calls: {}", self.total_tool_calls));
        report.push_str(&format!("\n   • Total Errors: {}", self.total_errors));
        
        // Per-segment details
        report.push_str(&format!("\n\n🔍 Segment Details:"));
        let mut segments: Vec<_> = self.segments.iter().collect();
        segments.sort_by_key(|(id, _)| *id);
        
        for (id, segment) in segments {
            report.push_str(&format!("\n\n   Segment {}:", id));
            report.push_str(&format!("\n      Status: {}", segment.state));
            report.push_str(&format!("\n      Workspace: {}", segment.workspace.display()));
            report.push_str(&format!("\n      Tokens: {}", segment.tokens_used));
            report.push_str(&format!("\n      Tool Calls: {}", segment.tool_calls));
            report.push_str(&format!("\n      Errors: {}", segment.errors));
            report.push_str(&format!("\n      Turn: {}/{}", segment.current_turn, segment.max_turns));
            
            if let Some(ref msg) = segment.last_message {
                report.push_str(&format!("\n      Last Message: {}", msg));
            }
            
            if let Some(ref err) = segment.error_message {
                report.push_str(&format!("\n      Error: {}", err));
            }
        }
        
        report.push_str(&format!("\n\n{}", "=".repeat(80)));
        
        report
    }
}
