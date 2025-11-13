//! Integration tests for g3-ensembles flock mode

use g3_ensembles::{FlockConfig, FlockMode};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Helper to create a test git repository with flock-requirements.md
fn create_test_project(name: &str) -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let project_path = temp_dir.path();

    // Initialize git repo
    let output = Command::new("git")
        .arg("init")
        .current_dir(project_path)
        .output()
        .expect("Failed to run git init");
    assert!(output.status.success(), "git init failed");

    // Configure git user (required for commits)
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(project_path)
        .output()
        .expect("Failed to configure git email");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(project_path)
        .output()
        .expect("Failed to configure git name");

    // Create flock-requirements.md
    let requirements = format!(
        "# {} Test Project\n\n\
        ## Module A\n\
        - Create a simple Rust library\n\
        - Add a function that returns \"Hello from Module A\"\n\
        - Write a unit test for the function\n\n\
        ## Module B\n\
        - Create another Rust library\n\
        - Add a function that returns \"Hello from Module B\"\n\
        - Write a unit test for the function\n",
        name
    );

    fs::write(project_path.join("flock-requirements.md"), requirements)
        .expect("Failed to write requirements");

    // Create a simple README
    fs::write(project_path.join("README.md"), format!("# {}\n", name))
        .expect("Failed to write README");

    // Create initial commit
    Command::new("git")
        .args(["add", "."])
        .current_dir(project_path)
        .output()
        .expect("Failed to git add");

    let output = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(project_path)
        .output()
        .expect("Failed to git commit");
    assert!(output.status.success(), "git commit failed");

    temp_dir
}

#[test]
 fn test_flock_config_validation() {
    let temp_dir = TempDir::new().unwrap();
    let project_path = temp_dir.path().to_path_buf();
    let workspace_path = temp_dir.path().join("workspace");

    // Should fail - not a git repo
    let result = FlockConfig::new(project_path.clone(), workspace_path.clone(), 2);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("must be a git repository"));

    // Initialize git repo
    Command::new("git")
        .arg("init")
        .current_dir(&project_path)
        .output()
        .expect("Failed to run git init");

    // Should fail - no flock-requirements.md
    let result = FlockConfig::new(project_path.clone(), workspace_path.clone(), 2);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("flock-requirements.md"));

    // Create flock-requirements.md
    fs::write(project_path.join("flock-requirements.md"), "# Test\n")
        .expect("Failed to write requirements");

    // Should succeed now
    let result = FlockConfig::new(project_path, workspace_path, 2);
    assert!(result.is_ok());
}

#[test]
fn test_flock_config_builder() {
    let project_dir = create_test_project("builder-test");
    let workspace_dir = TempDir::new().unwrap();

    let config = FlockConfig::new(
        project_dir.path().to_path_buf(),
        workspace_dir.path().to_path_buf(),
        2,
    )
    .expect("Failed to create config")
    .with_max_turns(15)
    .with_g3_binary(PathBuf::from("/custom/g3"));

    assert_eq!(config.num_segments, 2);
    assert_eq!(config.max_turns, 15);
    assert_eq!(config.g3_binary, Some(PathBuf::from("/custom/g3")));
}

#[test]
fn test_workspace_creation() {
    let project_dir = create_test_project("workspace-test");
    let workspace_dir = TempDir::new().unwrap();

    let config = FlockConfig::new(
        project_dir.path().to_path_buf(),
        workspace_dir.path().to_path_buf(),
        2,
    )
    .expect("Failed to create config");

    // Create FlockMode instance
    let _flock = FlockMode::new(config).expect("Failed to create FlockMode");

    // Verify workspace directory structure will be created
    // (We can't run the full flock without LLM access, but we can test the setup)
    assert!(project_dir.path().join(".git").exists());
    assert!(project_dir.path().join("flock-requirements.md").exists());
}

#[test]
fn test_git_clone_functionality() {
    let project_dir = create_test_project("clone-test");
    let workspace_dir = TempDir::new().unwrap();

    // Manually test git cloning (what flock mode does internally)
    let segment_dir = workspace_dir.path().join("segment-1");

    let output = Command::new("git")
        .arg("clone")
        .arg(project_dir.path())
        .arg(&segment_dir)
        .output()
        .expect("Failed to run git clone");

    assert!(output.status.success(), "git clone failed: {:?}", output);

    // Verify the clone
    assert!(segment_dir.exists());
    assert!(segment_dir.join(".git").exists());
    assert!(segment_dir.join("flock-requirements.md").exists());
    assert!(segment_dir.join("README.md").exists());

    // Verify it's a proper git repo
    let output = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&segment_dir)
        .output()
        .expect("Failed to run git log");

    assert!(output.status.success());
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(log.contains("Initial commit"));
}

#[test]
fn test_multiple_segment_clones() {
    let project_dir = create_test_project("multi-clone-test");
    let workspace_dir = TempDir::new().unwrap();

    // Clone multiple segments
    for i in 1..=2 {
        let segment_dir = workspace_dir.path().join(format!("segment-{}", i));

        let output = Command::new("git")
            .arg("clone")
            .arg(project_dir.path())
            .arg(&segment_dir)
            .output()
            .expect("Failed to run git clone");

        assert!(output.status.success(), "git clone {} failed", i);
        assert!(segment_dir.exists());
        assert!(segment_dir.join(".git").exists());
        assert!(segment_dir.join("flock-requirements.md").exists());
    }

    // Verify both segments exist and are independent
    let segment1 = workspace_dir.path().join("segment-1");
    let segment2 = workspace_dir.path().join("segment-2");

    assert!(segment1.exists());
    assert!(segment2.exists());

    // Modify segment 1
    fs::write(segment1.join("test.txt"), "segment 1")
        .expect("Failed to write to segment 1");

    // Verify segment 2 is unaffected
    assert!(!segment2.join("test.txt").exists());
}

#[test]
fn test_segment_requirements_creation() {
    let project_dir = create_test_project("segment-req-test");
    let workspace_dir = TempDir::new().unwrap();

    // Clone a segment
    let segment_dir = workspace_dir.path().join("segment-1");
    Command::new("git")
        .arg("clone")
        .arg(project_dir.path())
        .arg(&segment_dir)
        .output()
        .expect("Failed to clone");

    // Create segment-requirements.md (what flock mode does)
    let segment_requirements = "# Module A\n\nImplement module A functionality\n";
    fs::write(segment_dir.join("segment-requirements.md"), segment_requirements)
        .expect("Failed to write segment requirements");

    // Verify it was created
    assert!(segment_dir.join("segment-requirements.md").exists());
    let content = fs::read_to_string(segment_dir.join("segment-requirements.md"))
        .expect("Failed to read segment requirements");
    assert!(content.contains("Module A"));
}

#[test]
fn test_status_file_operations() {
    use g3_ensembles::FlockStatus;

    let temp_dir = TempDir::new().unwrap();
    let status_file = temp_dir.path().join("flock-status.json");

    // Create a status
    let status = FlockStatus::new(
        "test-session".to_string(),
        PathBuf::from("/test/project"),
        PathBuf::from("/test/workspace"),
        2,
    );

    // Save to file
    status
        .save_to_file(&status_file)
        .expect("Failed to save status");

    // Verify file exists
    assert!(status_file.exists());

    // Load from file
    let loaded = FlockStatus::load_from_file(&status_file).expect("Failed to load status");

    assert_eq!(loaded.session_id, "test-session");
    assert_eq!(loaded.num_segments, 2);
}

#[test]
fn test_json_extraction() {
    // Test the JSON extraction logic used in partition_requirements
    let test_cases = vec![
        (
            "Here is the result: [{\"module_name\": \"test\"}]",
            Some("[{\"module_name\": \"test\"}]"),
        ),
        (
            "```json\n[{\"module_name\": \"test\"}]\n```",
            Some("[{\"module_name\": \"test\"}]"),
        ),
        (
            "Some text before\n[{\"a\": 1}, {\"b\": 2}]\nSome text after",
            Some("[{\"a\": 1}, {\"b\": 2}]"),
        ),
        ("No JSON here", None),
    ];

    for (input, expected) in test_cases {
        let result = extract_json_array(input);
        match expected {
            Some(exp) => {
                assert!(result.is_some(), "Failed to extract from: {}", input);
                assert_eq!(result.unwrap(), exp);
            }
            None => {
                assert!(result.is_none(), "Should not extract from: {}", input);
            }
        }
    }
}

// Helper function to extract JSON array (mimics the logic in flock.rs)
fn extract_json_array(output: &str) -> Option<String> {
    if let Some(start) = output.find('[') {
        if let Some(end) = output.rfind(']') {
            if end > start {
                return Some(output[start..=end].to_string());
            }
        }
    }
    None
}

#[test]
fn test_partition_json_parsing() {
    // Test parsing of partition JSON
    let json = r#"[
        {
            "module_name": "core-library",
            "requirements": "Build the core library with basic functionality",
            "dependencies": []
        },
        {
            "module_name": "cli-tool",
            "requirements": "Create a CLI tool that uses the core library",
            "dependencies": ["core-library"]
        }
    ]"#;

    let partitions: Vec<serde_json::Value> =
        serde_json::from_str(json).expect("Failed to parse JSON");

    assert_eq!(partitions.len(), 2);
    assert_eq!(partitions[0]["module_name"], "core-library");
    assert_eq!(partitions[1]["module_name"], "cli-tool");
    assert_eq!(partitions[1]["dependencies"][0], "core-library");
}

#[test]
fn test_requirements_file_content() {
    let project_dir = create_test_project("content-test");

    let requirements_path = project_dir.path().join("flock-requirements.md");
    let content = fs::read_to_string(&requirements_path).expect("Failed to read requirements");

    // Verify content structure
    assert!(content.contains("# content-test Test Project"));
    assert!(content.contains("## Module A"));
    assert!(content.contains("## Module B"));
    assert!(content.contains("Hello from Module A"));
    assert!(content.contains("Hello from Module B"));
}

#[test]
fn test_git_repo_independence() {
    let project_dir = create_test_project("independence-test");
    let workspace_dir = TempDir::new().unwrap();

    // Clone two segments
    let segment1 = workspace_dir.path().join("segment-1");
    let segment2 = workspace_dir.path().join("segment-2");

    Command::new("git")
        .arg("clone")
        .arg(project_dir.path())
        .arg(&segment1)
        .output()
        .expect("Failed to clone segment 1");

    Command::new("git")
        .arg("clone")
        .arg(project_dir.path())
        .arg(&segment2)
        .output()
        .expect("Failed to clone segment 2");

    // Make a commit in segment 1
    fs::write(segment1.join("file1.txt"), "content 1").expect("Failed to write file1");

    Command::new("git")
        .args(["add", "file1.txt"])
        .current_dir(&segment1)
        .output()
        .expect("Failed to git add");

    Command::new("git")
        .args(["commit", "-m", "Add file1"])
        .current_dir(&segment1)
        .output()
        .expect("Failed to commit in segment 1");

    // Make a different commit in segment 2
    fs::write(segment2.join("file2.txt"), "content 2").expect("Failed to write file2");

    Command::new("git")
        .args(["add", "file2.txt"])
        .current_dir(&segment2)
        .output()
        .expect("Failed to git add");

    Command::new("git")
        .args(["commit", "-m", "Add file2"])
        .current_dir(&segment2)
        .output()
        .expect("Failed to commit in segment 2");

    // Verify they have different commits
    let log1 = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&segment1)
        .output()
        .expect("Failed to get log 1");

    let log2 = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&segment2)
        .output()
        .expect("Failed to get log 2");

    let log1_str = String::from_utf8_lossy(&log1.stdout);
    let log2_str = String::from_utf8_lossy(&log2.stdout);

    assert!(log1_str.contains("Add file1"));
    assert!(!log1_str.contains("Add file2"));
    assert!(log2_str.contains("Add file2"));
    assert!(!log2_str.contains("Add file1"));

    // Verify files exist only in their respective segments
    assert!(segment1.join("file1.txt").exists());
    assert!(!segment1.join("file2.txt").exists());
    assert!(segment2.join("file2.txt").exists());
    assert!(!segment2.join("file1.txt").exists());
}
