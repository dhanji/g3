use g3_core::Agent;
use g3_core::ui_writer::NullUiWriter;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;


/// Helper to create a test agent in a temporary directory
async fn create_test_agent_in_dir(temp_dir: &TempDir) -> Agent<NullUiWriter> {
    // Change to temp directory
    std::env::set_current_dir(temp_dir.path()).unwrap();
    
    // Create a minimal config
    let config = g3_config::Config::default();
    let ui_writer = NullUiWriter;
    
    Agent::new(config, ui_writer).await.unwrap()
}

/// Helper to get todo.g3.md path in temp directory
fn get_todo_path(temp_dir: &TempDir) -> PathBuf {
    temp_dir.path().join("todo.g3.md")
}

#[tokio::test]
#[serial]
async fn test_todo_write_creates_file() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    let todo_path = get_todo_path(&temp_dir);
    
    // Initially, todo.g3.md should not exist
    assert!(!todo_path.exists(), "todo.g3.md should not exist initially");
    
    // Create a tool call to write TODO
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": "- [ ] Task 1\n- [ ] Task 2\n- [x] Task 3"
        }),
    };
    
    // Execute the tool
    let result = agent.execute_tool(&tool_call).await.unwrap();
    
    // Should report success
    assert!(result.contains("✅"), "Should report success: {}", result);
    assert!(result.contains("todo.g3.md"), "Should mention todo.g3.md: {}", result);
    
    // File should now exist
    assert!(todo_path.exists(), "todo.g3.md should exist after write");
    
    // File should contain the correct content
    let content = fs::read_to_string(&todo_path).unwrap();
    assert_eq!(content, "- [ ] Task 1\n- [ ] Task 2\n- [x] Task 3");
}

#[tokio::test]
#[serial]
async fn test_todo_read_from_file() {
    let temp_dir = TempDir::new().unwrap();
    let todo_path = get_todo_path(&temp_dir);
    
    // Pre-create a todo.g3.md file
    let test_content = "# My TODO\n\n- [ ] First task\n- [x] Completed task";
    fs::write(&todo_path, test_content).unwrap();
    
    // Create agent (should load from file)
    let agent = create_test_agent_in_dir(&temp_dir).await;
    
    // Create a tool call to read TODO
    let tool_call = g3_core::ToolCall {
        tool: "todo_read".to_string(),
        args: serde_json::json!({}),
    };
    
    // Execute the tool
    let result = agent.execute_tool(&tool_call).await.unwrap();
    
    // Should contain the TODO content
    assert!(result.contains("📝 TODO list:"), "Should have TODO list header: {}", result);
    assert!(result.contains("First task"), "Should contain first task: {}", result);
    assert!(result.contains("Completed task"), "Should contain completed task: {}", result);
}

#[tokio::test]
#[serial]
async fn test_todo_read_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    
    // Create a tool call to read TODO (file doesn't exist)
    let tool_call = g3_core::ToolCall {
        tool: "todo_read".to_string(),
        args: serde_json::json!({}),
    };
    
    // Execute the tool
    let result = agent.execute_tool(&tool_call).await.unwrap();
    
    // Should report empty
    assert!(result.contains("empty"), "Should report empty: {}", result);
}

#[tokio::test]
#[serial]
async fn test_todo_persistence_across_agents() {
    let temp_dir = TempDir::new().unwrap();
    let todo_path = get_todo_path(&temp_dir);
    
    // Agent 1: Write TODO
    {
        let agent = create_test_agent_in_dir(&temp_dir).await;
        let tool_call = g3_core::ToolCall {
            tool: "todo_write".to_string(),
            args: serde_json::json!({
                "content": "- [ ] Persistent task\n- [x] Done task"
            }),
        };
        agent.execute_tool(&tool_call).await.unwrap();
    }
    
    // Verify file exists
    assert!(todo_path.exists(), "todo.g3.md should persist after agent drops");
    
    // Agent 2: Read TODO (new agent instance)
    {
        let agent = create_test_agent_in_dir(&temp_dir).await;
        let tool_call = g3_core::ToolCall {
            tool: "todo_read".to_string(),
            args: serde_json::json!({}),
        };
        let result = agent.execute_tool(&tool_call).await.unwrap();
        
        // Should read the persisted content
        assert!(result.contains("Persistent task"), "Should read persisted task: {}", result);
        assert!(result.contains("Done task"), "Should read done task: {}", result);
    }
}

#[tokio::test]
#[serial]
async fn test_todo_update_preserves_file() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    let todo_path = get_todo_path(&temp_dir);
    
    // Write initial TODO
    let write_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": "- [ ] Task 1\n- [ ] Task 2"
        }),
    };
    agent.execute_tool(&write_call).await.unwrap();
    
    // Update TODO
    let update_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": "- [x] Task 1\n- [ ] Task 2\n- [ ] Task 3"
        }),
    };
    agent.execute_tool(&update_call).await.unwrap();
    
    // Verify file has updated content
    let content = fs::read_to_string(&todo_path).unwrap();
    assert_eq!(content, "- [x] Task 1\n- [ ] Task 2\n- [ ] Task 3");
}

#[tokio::test]
#[serial]
async fn test_todo_handles_large_content() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    let todo_path = get_todo_path(&temp_dir);
    
    // Create a large TODO (but under the 50k limit)
    let mut large_content = String::from("# Large TODO\n\n");
    for i in 0..100 {
        large_content.push_str(&format!("- [ ] Task {} with a long description that exceeds normal line lengths\n", i));
    }
    
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": large_content
        }),
    };
    
    let result = agent.execute_tool(&tool_call).await.unwrap();
    assert!(result.contains("✅"), "Should handle large content: {}", result);
    
    // Verify file contains all content
    let file_content = fs::read_to_string(&todo_path).unwrap();
    assert_eq!(file_content, large_content);
    assert!(file_content.contains("Task 99"), "Should contain all tasks");
}

#[tokio::test]
#[serial]
async fn test_todo_respects_size_limit() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    
    // Create content that exceeds the default 50k limit
    let huge_content = "x".repeat(60_000);
    
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": huge_content
        }),
    };
    
    let result = agent.execute_tool(&tool_call).await.unwrap();
    
    // Should reject content that's too large
    assert!(result.contains("❌"), "Should reject oversized content: {}", result);
    assert!(result.contains("too large"), "Should mention size limit: {}", result);
}

#[tokio::test]
#[serial]
async fn test_todo_agent_initialization_loads_file() {
    let temp_dir = TempDir::new().unwrap();
    let todo_path = get_todo_path(&temp_dir);
    
    // Pre-create todo.g3.md before agent initialization
    let initial_content = "- [ ] Pre-existing task";
    fs::write(&todo_path, initial_content).unwrap();
    
    // Create agent - should load the file during initialization
    let agent = create_test_agent_in_dir(&temp_dir).await;
    
    // Read TODO - should return the pre-existing content
    let tool_call = g3_core::ToolCall {
        tool: "todo_read".to_string(),
        args: serde_json::json!({}),
    };
    
    let result = agent.execute_tool(&tool_call).await.unwrap();
    assert!(result.contains("Pre-existing task"), "Should load file on init: {}", result);
}

#[tokio::test]
#[serial]
async fn test_todo_handles_unicode_content() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    let todo_path = get_todo_path(&temp_dir);
    
    // Create TODO with unicode characters
    let unicode_content = "- [ ] 日本語タスク\n- [ ] Émoji task 🚀\n- [x] Ελληνικά task";
    
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": unicode_content
        }),
    };
    
    agent.execute_tool(&tool_call).await.unwrap();
    
    // Verify file preserves unicode
    let file_content = fs::read_to_string(&todo_path).unwrap();
    assert_eq!(file_content, unicode_content);
    
    // Verify reading back works
    let read_call = g3_core::ToolCall {
        tool: "todo_read".to_string(),
        args: serde_json::json!({}),
    };
    
    let result = agent.execute_tool(&read_call).await.unwrap();
    assert!(result.contains("日本語"), "Should preserve Japanese: {}", result);
    assert!(result.contains("🚀"), "Should preserve emoji: {}", result);
    assert!(result.contains("Ελληνικά"), "Should preserve Greek: {}", result);
}

#[tokio::test]
#[serial]
async fn test_todo_empty_content_creates_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    let todo_path = get_todo_path(&temp_dir);
    
    // Write empty TODO
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": ""
        }),
    };
    
    agent.execute_tool(&tool_call).await.unwrap();
    
    // File should exist but be empty
    assert!(todo_path.exists(), "Empty todo.g3.md should create file");
    let content = fs::read_to_string(&todo_path).unwrap();
    assert_eq!(content, "");
}

#[tokio::test]
#[serial]
async fn test_todo_whitespace_only_content() {
    let temp_dir = TempDir::new().unwrap();
    let agent = create_test_agent_in_dir(&temp_dir).await;
    
    // Write whitespace-only TODO
    let tool_call = g3_core::ToolCall {
        tool: "todo_write".to_string(),
        args: serde_json::json!({
            "content": "   \n\n  \t  \n"
        }),
    };
    
    agent.execute_tool(&tool_call).await.unwrap();
    
    // Read it back
    let read_call = g3_core::ToolCall {
        tool: "todo_read".to_string(),
        args: serde_json::json!({}),
    };
    
    let result = agent.execute_tool(&read_call).await.unwrap();
    
    // Should report as empty (whitespace is trimmed)
    assert!(result.contains("empty"), "Whitespace-only should be empty: {}", result);
}
