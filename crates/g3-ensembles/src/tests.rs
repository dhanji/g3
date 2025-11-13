//! Unit tests for g3-ensembles

#[cfg(test)]
mod tests {
    use crate::status::{FlockStatus, SegmentState, SegmentStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    #[test]
    fn test_segment_state_display() {
        assert_eq!(format!("{}", SegmentState::Pending), "⏳ Pending");
        assert_eq!(format!("{}", SegmentState::Running), "🔄 Running");
        assert_eq!(format!("{}", SegmentState::Completed), "✅ Completed");
        assert_eq!(format!("{}", SegmentState::Failed), "❌ Failed");
        assert_eq!(format!("{}", SegmentState::Cancelled), "⚠️  Cancelled");
    }

    #[test]
    fn test_flock_status_creation() {
        let status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            3,
        );

        assert_eq!(status.session_id, "test-session");
        assert_eq!(status.num_segments, 3);
        assert_eq!(status.segments.len(), 0);
        assert_eq!(status.total_tokens, 0);
        assert_eq!(status.total_tool_calls, 0);
        assert_eq!(status.total_errors, 0);
        assert!(status.completed_at.is_none());
    }

    #[test]
    fn test_segment_status_update() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            2,
        );

        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 2,
            current_turn: 5,
            max_turns: 10,
            last_message: Some("Done".to_string()),
            error_message: None,
        };

        status.update_segment(1, segment1);

        assert_eq!(status.segments.len(), 1);
        assert_eq!(status.total_tokens, 1000);
        assert_eq!(status.total_tool_calls, 50);
        assert_eq!(status.total_errors, 2);
    }

    #[test]
    fn test_multiple_segment_updates() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            2,
        );

        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 2,
            current_turn: 5,
            max_turns: 10,
            last_message: Some("Done".to_string()),
            error_message: None,
        };

        let segment2 = SegmentStatus {
            segment_id: 2,
            workspace: PathBuf::from("/test/workspace/segment-2"),
            state: SegmentState::Failed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 500,
            tool_calls: 25,
            errors: 5,
            current_turn: 3,
            max_turns: 10,
            last_message: Some("Error".to_string()),
            error_message: Some("Test error".to_string()),
        };

        status.update_segment(1, segment1);
        status.update_segment(2, segment2);

        assert_eq!(status.segments.len(), 2);
        assert_eq!(status.total_tokens, 1500);
        assert_eq!(status.total_tool_calls, 75);
        assert_eq!(status.total_errors, 7);
    }

    #[test]
    fn test_is_complete() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            2,
        );

        // Not complete - no segments
        assert!(!status.is_complete());

        // Add one completed segment
        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 0,
            current_turn: 5,
            max_turns: 10,
            last_message: None,
            error_message: None,
        };
        status.update_segment(1, segment1);

        // Still not complete - only 1 of 2 segments
        assert!(!status.is_complete());

        // Add second segment (running)
        let segment2 = SegmentStatus {
            segment_id: 2,
            workspace: PathBuf::from("/test/workspace/segment-2"),
            state: SegmentState::Running,
            started_at: Utc::now(),
            completed_at: None,
            tokens_used: 500,
            tool_calls: 25,
            errors: 0,
            current_turn: 3,
            max_turns: 10,
            last_message: None,
            error_message: None,
        };
        status.update_segment(2, segment2);

        // Still not complete - segment 2 is running
        assert!(!status.is_complete());

        // Update segment 2 to completed
        let segment2_done = SegmentStatus {
            segment_id: 2,
            workspace: PathBuf::from("/test/workspace/segment-2"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 500,
            tool_calls: 25,
            errors: 0,
            current_turn: 5,
            max_turns: 10,
            last_message: None,
            error_message: None,
        };
        status.update_segment(2, segment2_done);

        // Now complete
        assert!(status.is_complete());
    }

    #[test]
    fn test_count_by_state() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            3,
        );

        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 0,
            current_turn: 5,
            max_turns: 10,
            last_message: None,
            error_message: None,
        };

        let segment2 = SegmentStatus {
            segment_id: 2,
            workspace: PathBuf::from("/test/workspace/segment-2"),
            state: SegmentState::Failed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 500,
            tool_calls: 25,
            errors: 5,
            current_turn: 3,
            max_turns: 10,
            last_message: None,
            error_message: Some("Error".to_string()),
        };

        let segment3 = SegmentStatus {
            segment_id: 3,
            workspace: PathBuf::from("/test/workspace/segment-3"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 800,
            tool_calls: 40,
            errors: 1,
            current_turn: 4,
            max_turns: 10,
            last_message: None,
            error_message: None,
        };

        status.update_segment(1, segment1);
        status.update_segment(2, segment2);
        status.update_segment(3, segment3);

        assert_eq!(status.count_by_state(SegmentState::Completed), 2);
        assert_eq!(status.count_by_state(SegmentState::Failed), 1);
        assert_eq!(status.count_by_state(SegmentState::Running), 0);
        assert_eq!(status.count_by_state(SegmentState::Pending), 0);
    }

    #[test]
    fn test_status_serialization() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            1,
        );

        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 2,
            current_turn: 5,
            max_turns: 10,
            last_message: Some("Done".to_string()),
            error_message: None,
        };

        status.update_segment(1, segment1);

        // Serialize to JSON
        let json = serde_json::to_string(&status).expect("Failed to serialize");
        assert!(json.contains("test-session"));
        assert!(json.contains("segment_id"));
        assert!(json.contains("Completed"));

        // Deserialize back
        let deserialized: FlockStatus =
            serde_json::from_str(&json).expect("Failed to deserialize");
        assert_eq!(deserialized.session_id, "test-session");
        assert_eq!(deserialized.segments.len(), 1);
        assert_eq!(deserialized.total_tokens, 1000);
    }

    #[test]
    fn test_report_generation() {
        let mut status = FlockStatus::new(
            "test-session".to_string(),
            PathBuf::from("/test/project"),
            PathBuf::from("/test/workspace"),
            2,
        );

        let segment1 = SegmentStatus {
            segment_id: 1,
            workspace: PathBuf::from("/test/workspace/segment-1"),
            state: SegmentState::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            tokens_used: 1000,
            tool_calls: 50,
            errors: 2,
            current_turn: 5,
            max_turns: 10,
            last_message: Some("Done".to_string()),
            error_message: None,
        };

        status.update_segment(1, segment1);

        let report = status.generate_report();

        // Check that report contains expected sections
        assert!(report.contains("FLOCK MODE SESSION REPORT"));
        assert!(report.contains("test-session"));
        assert!(report.contains("Segment Status:"));
        assert!(report.contains("Aggregate Metrics:"));
        assert!(report.contains("Segment Details:"));
        assert!(report.contains("Total Tokens: 1000"));
        assert!(report.contains("Total Tool Calls: 50"));
        assert!(report.contains("Total Errors: 2"));
    }
}
