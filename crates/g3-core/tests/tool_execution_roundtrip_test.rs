//! Tool Execution Round-Trip Integration Tests
//!
//! CHARACTERIZATION: These tests verify that tools execute correctly through
//! the Agent interface, testing the full round-trip from tool call to result.
//!
//! What these tests protect:
//! - File operations (read, write, str_replace) work end-to-end
//! - Shell command execution produces expected output
//! - TODO operations persist correctly
//! - Error handling for invalid inputs
//!
//! What these tests intentionally do NOT assert:
//! - Internal implementation details of tools
//! - Specific formatting of success messages (only key content)
//! - UI writer behavior (uses NullUiWriter)

use g3_core::ui_writer::NullUiWriter;
use g3_core::{Agent, ToolCall};
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

// =============================================================================
// Test Helpers
// =============================================================================

/// Create a test agent in a temporary directory
async fn create_test_agent(temp_dir: &TempDir) -> Agent<NullUiWriter> {
    std::env::set_current_dir(temp_dir.path()).unwrap();
    let config = g3_config::Config::default();
    let ui_writer = NullUiWriter;
    Agent::new(config, ui_writer).await.unwrap()
}

/// Create a ToolCall with the given tool name and arguments
fn make_tool_call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        tool: tool.to_string(),
        args,
        id: String::new(),
    }
}

// =============================================================================
// Test: read_file tool execution
// =============================================================================

mod read_file_execution {
    use super::*;

    /// Test reading an existing file
    #[tokio::test]
    #[serial]
    async fn test_read_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "Hello, World!\nLine 2\nLine 3").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "read_file",
            serde_json::json!({ "file_path": test_file.to_string_lossy() }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("Hello, World!"), "Should contain file content: {}", result);
        assert!(result.contains("Line 2"), "Should contain all lines: {}", result);
    }

    /// Test reading a non-existent file returns error
    #[tokio::test]
    #[serial]
    async fn test_read_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "read_file",
            serde_json::json!({ "file_path": "/nonexistent/path/file.txt" }),
        );
        
        let result = agent.execute_tool(&tool_call).await;
        
        // Should return an error or error message
        assert!(
            result.is_err() || result.as_ref().unwrap().contains("error") || result.as_ref().unwrap().contains("not found") || result.as_ref().unwrap().contains("No such file"),
            "Should indicate file not found: {:?}", result
        );
    }

    /// Test reading with character range
    #[tokio::test]
    #[serial]
    async fn test_read_file_with_range() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "0123456789ABCDEF").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "read_file",
            serde_json::json!({
                "file_path": test_file.to_string_lossy(),
                "start": 5,
                "end": 10
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        // Should contain the substring from position 5 to 10
        assert!(result.contains("56789"), "Should contain range content: {}", result);
    }
}

// =============================================================================
// Test: write_file tool execution
// =============================================================================

mod write_file_execution {
    use super::*;

    /// Test writing a new file
    #[tokio::test]
    #[serial]
    async fn test_write_new_file() {
        let temp_dir = TempDir::new().unwrap();
        let new_file = temp_dir.path().join("new_file.txt");
        
        assert!(!new_file.exists(), "File should not exist initially");
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "write_file",
            serde_json::json!({
                "file_path": new_file.to_string_lossy(),
                "content": "New content here"
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        // Should report success
        assert!(result.contains("✅") || result.to_lowercase().contains("success") || result.to_lowercase().contains("wrote"),
            "Should report success: {}", result);
        
        // File should now exist with correct content
        assert!(new_file.exists(), "File should exist after write");
        let content = fs::read_to_string(&new_file).unwrap();
        assert_eq!(content, "New content here");
    }

    /// Test overwriting an existing file
    #[tokio::test]
    #[serial]
    async fn test_overwrite_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("existing.txt");
        fs::write(&test_file, "Original content").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "write_file",
            serde_json::json!({
                "file_path": test_file.to_string_lossy(),
                "content": "Replaced content"
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("✅") || result.to_lowercase().contains("success") || result.to_lowercase().contains("wrote"),
            "Should report success: {}", result);
        
        let content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "Replaced content");
    }

    /// Test writing creates parent directories
    #[tokio::test]
    #[serial]
    async fn test_write_creates_parent_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let nested_file = temp_dir.path().join("a/b/c/nested.txt");
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "write_file",
            serde_json::json!({
                "file_path": nested_file.to_string_lossy(),
                "content": "Nested content"
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("✅") || result.to_lowercase().contains("success") || result.to_lowercase().contains("wrote"),
            "Should report success: {}", result);
        
        assert!(nested_file.exists(), "Nested file should exist");
        let content = fs::read_to_string(&nested_file).unwrap();
        assert_eq!(content, "Nested content");
    }
}

// =============================================================================
// Test: shell tool execution
// =============================================================================

mod shell_execution {
    use super::*;

    /// Test simple echo command
    #[tokio::test]
    #[serial]
    async fn test_shell_echo() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "shell",
            serde_json::json!({ "command": "echo 'hello world'" }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("hello world"), "Should contain echo output: {}", result);
    }

    /// Test command that produces multi-line output
    #[tokio::test]
    #[serial]
    async fn test_shell_multiline_output() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "shell",
            serde_json::json!({ "command": "echo 'line1'; echo 'line2'; echo 'line3'" }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("line1"), "Should contain line1: {}", result);
        assert!(result.contains("line2"), "Should contain line2: {}", result);
        assert!(result.contains("line3"), "Should contain line3: {}", result);
    }

    /// Test command that fails
    #[tokio::test]
    #[serial]
    async fn test_shell_failing_command() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "shell",
            serde_json::json!({ "command": "exit 1" }),
        );
        
        let result = agent.execute_tool(&tool_call).await;
        
        // Should indicate failure (either error or non-zero exit)
        assert!(
            result.is_err() || result.as_ref().unwrap().contains("exit") || result.as_ref().unwrap().contains("failed") || result.as_ref().unwrap().contains("error"),
            "Should indicate command failure: {:?}", result
        );
    }

    /// Test command with working directory context
    #[tokio::test]
    #[serial]
    async fn test_shell_pwd() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        
        let tool_call = make_tool_call(
            "shell",
            serde_json::json!({ "command": "pwd" }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        // Should show the temp directory path
        let temp_path = temp_dir.path().to_string_lossy();
        assert!(result.contains(&*temp_path) || result.contains("private"), 
            "Should show current directory: {} (expected to contain {})", result, temp_path);
    }
}

// =============================================================================
// Test: str_replace tool execution
// =============================================================================

mod str_replace_execution {
    use super::*;

    /// Test applying a simple diff
    #[tokio::test]
    #[serial]
    async fn test_str_replace_simple() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "line 1\nold line\nline 3\n").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let diff = "@@ -1,3 +1,3 @@\n line 1\n-old line\n+new line\n line 3\n";
        
        let tool_call = make_tool_call(
            "str_replace",
            serde_json::json!({
                "file_path": test_file.to_string_lossy(),
                "diff": diff
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("✅") || result.to_lowercase().contains("applied") || result.to_lowercase().contains("success"),
            "Should report success: {}", result);
        
        let content = fs::read_to_string(&test_file).unwrap();
        assert!(content.contains("new line"), "Should contain new content: {}", content);
        assert!(!content.contains("old line"), "Should not contain old content: {}", content);
    }

    /// Test diff that adds lines
    #[tokio::test]
    #[serial]
    async fn test_str_replace_add_lines() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "line 1\nline 3\n").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let diff = "@@ -1,2 +1,3 @@\n line 1\n+line 2\n line 3\n";
        
        let tool_call = make_tool_call(
            "str_replace",
            serde_json::json!({
                "file_path": test_file.to_string_lossy(),
                "diff": diff
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        assert!(result.contains("✅") || result.to_lowercase().contains("applied"),
            "Should report success: {}", result);
        
        let content = fs::read_to_string(&test_file).unwrap();
        assert!(content.contains("line 2"), "Should contain added line: {}", content);
    }

    /// Test diff with pattern not found
    #[tokio::test]
    #[serial]
    async fn test_str_replace_pattern_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "actual content\n").unwrap();
        
        let mut agent = create_test_agent(&temp_dir).await;
        
        let diff = "@@ -1,1 +1,1 @@\n-nonexistent pattern\n+replacement\n";
        
        let tool_call = make_tool_call(
            "str_replace",
            serde_json::json!({
                "file_path": test_file.to_string_lossy(),
                "diff": diff
            }),
        );
        
        let result = agent.execute_tool(&tool_call).await;
        
        // Should indicate pattern not found
        assert!(
            result.is_err() || result.as_ref().unwrap().to_lowercase().contains("not found") || result.as_ref().unwrap().to_lowercase().contains("pattern") || result.as_ref().unwrap().to_lowercase().contains("error"),
            "Should indicate pattern not found: {:?}", result
        );
    }
}

// =============================================================================
// Test: TODO tool execution
// =============================================================================

mod plan_execution {
    use super::*;

    /// plan_write persists a slim plan; plan_read/plan_approve no longer exist.
    #[tokio::test]
    #[serial]
    async fn test_plan_write_persists_slim_plan() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        agent.init_session_id_for_test("plan-test");
        let sid = agent.get_session_id().unwrap().to_string();

        let write_call = make_tool_call(
            "plan_write",
            serde_json::json!({ "plan": "plan_id: test-plan\nitems:\n  - {id: I1, description: Task 1, state: todo}" }),
        );
        let write_result = agent.execute_tool(&write_call).await.unwrap();
        assert!(write_result.starts_with("✅"), "Write should succeed: {}", write_result);

        let plan = g3_core::tools::plan::read_plan(&sid).unwrap().expect("persisted");
        assert_eq!(plan.items[0].description, "Task 1");

        let read = agent.execute_tool(&make_tool_call("plan_read", serde_json::json!({}))).await.unwrap();
        assert!(read.contains("Unknown tool"), "plan_read is gone: {}", read);
    }
}


// =============================================================================
// Test: write_envelope with analysis/rulespec.yaml datalog integration
// (independent of plans since 2026-11-26)
// =============================================================================

mod envelope_datalog_integration {
    use super::*;

    /// Test: write_envelope compiles datalog rules on-the-fly from analysis/rulespec.yaml
    /// and writes .dl + evaluation files to session dir via verify_envelope
    #[tokio::test]
    #[serial]
    async fn test_envelope_with_analysis_rulespec() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        agent.init_session_id_for_test("datalog-rulespec-test");
        let session_id = agent.get_session_id().unwrap().to_string();

        // A file the facts point at
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "// test file").unwrap();

        // Write analysis/rulespec.yaml
        let analysis_dir = temp_dir.path().join("analysis");
        fs::create_dir_all(&analysis_dir).unwrap();
        fs::write(
            analysis_dir.join("rulespec.yaml"),
            "claims:
  - name: feature_done
    selector: feature.done
predicates:
  - claim: feature_done
    rule: exists
    source: task_prompt
    notes: Feature must be marked done
",
        )
        .unwrap();

        // Call write_envelope - this triggers verify_envelope which writes artifacts
        let envelope_call = make_tool_call(
            "write_envelope",
            serde_json::json!({
                "facts": "facts:\n  feature:\n    done: true\n    capabilities: [handle_csv, handle_tsv]\n    file: src/lib.rs"
            }),
        );
        let result = agent.execute_tool(&envelope_call).await.unwrap();
        assert!(result.contains("Envelope written"), "Should confirm envelope written: {}", result);

        // Check that .dl and evaluation files were written to session dir
        let session_dir = temp_dir
            .path()
            .join(".g3")
            .join("sessions")
            .join(&session_id);
        let dl_path = session_dir.join("rulespec.compiled.dl");
        let eval_path = session_dir.join("datalog_evaluation.txt");

        assert!(dl_path.exists(), "Compiled .dl file should exist at {}", dl_path.display());
        assert!(eval_path.exists(), "Evaluation report should exist at {}", eval_path.display());

        // Verify evaluation content shows pass
        let eval_content = fs::read_to_string(&eval_path).unwrap();
        assert!(eval_content.contains("satisfied"),
            "Evaluation should show passing results: {}", eval_content);

        // Verify facts were actually extracted (not zero)
        assert!(!eval_content.contains("Facts extracted: 0"),
            "Should have extracted facts: {}", eval_content);
    }

    /// Test: write_envelope works gracefully when analysis/rulespec.yaml is absent
    #[tokio::test]
    #[serial]
    async fn test_envelope_without_rulespec() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;
        agent.init_session_id_for_test("datalog-no-rulespec-test");
        let session_id = agent.get_session_id().unwrap().to_string();

        // Do NOT create analysis/rulespec.yaml
        let envelope_call = make_tool_call(
            "write_envelope",
            serde_json::json!({ "facts": "facts:\n  feature:\n    done: true" }),
        );
        let result = agent.execute_tool(&envelope_call).await.unwrap();
        assert!(result.contains("Envelope written"), "Should confirm envelope written: {}", result);

        let session_dir = temp_dir.path().join(".g3").join("sessions").join(&session_id);
        assert!(session_dir.join("envelope.yaml").exists(), "envelope written");
        assert!(!session_dir.join("rulespec.compiled.dl").exists(),
            "No .dl file should exist without rulespec");
        assert!(!session_dir.join("datalog_evaluation.txt").exists(),
            "No evaluation file should exist without rulespec");
    }

    /// Test: write_envelope with rulespec predicate that fails shows failure
    #[tokio::test]
    #[serial]
    async fn test_envelope_rulespec_failure() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        agent.init_session_id_for_test("datalog-fail-test");
        let session_id = agent.get_session_id().unwrap().to_string();

        // A file the facts point at
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "// test file").unwrap();

        // Write a rulespec that will FAIL (expects a fact that doesn't exist)
        let analysis_dir = temp_dir.path().join("analysis");
        fs::create_dir_all(&analysis_dir).unwrap();
        fs::write(
            analysis_dir.join("rulespec.yaml"),
            "claims:
  - name: missing_feature
    selector: nonexistent.field
predicates:
  - claim: missing_feature
    rule: exists
    source: task_prompt
    notes: This field does not exist in the envelope
",
        )
        .unwrap();

        // Call write_envelope - this triggers verify_envelope
        let envelope_call = make_tool_call(
            "write_envelope",
            serde_json::json!({
                "facts": "facts:\n  feature:\n    done: true\n    capabilities: [handle_csv, handle_tsv]\n    file: src/lib.rs"
            }),
        );
        agent.execute_tool(&envelope_call).await.unwrap();

        // Check evaluation file shows failure
        let session_dir = temp_dir
            .path()
            .join(".g3")
            .join("sessions")
            .join(&session_id);
        let eval_path = session_dir.join("datalog_evaluation.txt");
        assert!(eval_path.exists(), "Evaluation report should exist");

        let eval_content = fs::read_to_string(&eval_path).unwrap();
        assert!(eval_content.contains("FAIL") || eval_content.contains("fail"),
            "Evaluation should show failing results: {}", eval_content);
    }

    /// Test: write_envelope with facts. prefix selectors in rulespec still extracts facts
    #[tokio::test]
    #[serial]
    async fn test_envelope_rulespec_with_facts_prefix_selectors() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        agent.init_session_id_for_test("datalog-facts-prefix-test");
        let session_id = agent.get_session_id().unwrap().to_string();

        // A file the facts point at
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "// test file").unwrap();

        // Write rulespec with facts. prefix selectors
        let analysis_dir = temp_dir.path().join("analysis");
        fs::create_dir_all(&analysis_dir).unwrap();
        fs::write(
            analysis_dir.join("rulespec.yaml"),
            "claims:\n  - name: feature_done\n    selector: facts.feature.done\n  - name: caps\n    selector: facts.feature.capabilities\npredicates:\n  - claim: feature_done\n    rule: exists\n    source: task_prompt\n    notes: Feature must be marked done\n  - claim: caps\n    rule: contains\n    value: handle_csv\n    source: task_prompt\n    notes: Must support CSV\n",
        )
        .unwrap();

        // Call write_envelope
        let envelope_call = make_tool_call(
            "write_envelope",
            serde_json::json!({
                "facts": "facts:\n  feature:\n    done: true\n    capabilities: [handle_csv, handle_tsv]\n    file: src/lib.rs"
            }),
        );
        let result = agent.execute_tool(&envelope_call).await.unwrap();
        assert!(result.contains("Envelope written"), "Should confirm envelope written: {}", result);

        // Check evaluation file
        let session_dir = temp_dir
            .path()
            .join(".g3")
            .join("sessions")
            .join(&session_id);
        let eval_path = session_dir.join("datalog_evaluation.txt");
        assert!(eval_path.exists(), "Evaluation report should exist");

        let eval_content = fs::read_to_string(&eval_path).unwrap();
        // Facts should be extracted despite facts. prefix selectors
        assert!(!eval_content.contains("Facts extracted: 0"),
            "Should extract facts even with facts. prefix selectors: {}", eval_content);
        assert!(eval_content.contains("satisfied"),
            "All predicates should pass: {}", eval_content);
    }

    /// Test: write_envelope with multiple claims and mixed pass/fail results
    #[tokio::test]
    #[serial]
    async fn test_envelope_mixed_pass_fail() {
        let temp_dir = TempDir::new().unwrap();
        let mut agent = create_test_agent(&temp_dir).await;

        agent.init_session_id_for_test("datalog-mixed-test");
        let session_id = agent.get_session_id().unwrap().to_string();

        // A file the facts point at
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "// test file").unwrap();

        // Write rulespec with one passing and one failing predicate
        let analysis_dir = temp_dir.path().join("analysis");
        fs::create_dir_all(&analysis_dir).unwrap();
        fs::write(
            analysis_dir.join("rulespec.yaml"),
            "claims:\n  - name: feature_done\n    selector: feature.done\n  - name: missing_thing\n    selector: nonexistent.field\npredicates:\n  - claim: feature_done\n    rule: exists\n    source: task_prompt\n    notes: This should pass\n  - claim: missing_thing\n    rule: exists\n    source: task_prompt\n    notes: This should fail\n",
        )
        .unwrap();

        // Call write_envelope
        let envelope_call = make_tool_call(
            "write_envelope",
            serde_json::json!({
                "facts": "facts:\n  feature:\n    done: true\n    capabilities: [handle_csv]\n    file: src/lib.rs"
            }),
        );
        let result = agent.execute_tool(&envelope_call).await.unwrap();
        assert!(result.contains("Envelope written"), "Should confirm envelope written: {}", result);
        // Should report mixed results
        assert!(result.contains("failed"), "Should report failures in tool output: {}", result);

        // Check evaluation file
        let session_dir = temp_dir
            .path()
            .join(".g3")
            .join("sessions")
            .join(&session_id);
        let eval_path = session_dir.join("datalog_evaluation.txt");
        assert!(eval_path.exists(), "Evaluation report should exist");

        let eval_content = fs::read_to_string(&eval_path).unwrap();
        // Should have extracted some facts (from the feature that exists)
        assert!(!eval_content.contains("Facts extracted: 0"),
            "Should extract some facts: {}", eval_content);
        // Should show 1 pass and 1 fail
        assert!(eval_content.contains("1/2") || (eval_content.contains("passed") && eval_content.contains("failed")),
            "Should show mixed results: {}", eval_content);
    }
}
