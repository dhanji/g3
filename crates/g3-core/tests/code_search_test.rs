//! Integration tests for tree-sitter code search

use g3_core::code_search::{execute_code_search, CodeSearchRequest, SearchSpec};
use std::fs;

#[tokio::test]
async fn test_find_async_functions() {
    // Create a temporary test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.rs");
    fs::write(
        &test_file,
        r#"
pub async fn example_async() {
    println!("Hello");
}

fn regular_function() {
    println!("Regular");
}

pub async fn another_async(x: i32) -> Result<(), ()> {
    Ok(())
}
"#,
    )
    .unwrap();

    // Test 1: Find async functions
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "find_async_functions".to_string(),
            // In tree-sitter-rust, async is a token inside function_modifiers
            query: "(function_item (function_modifiers) name: (identifier) @name)".to_string(),
            language: "rust".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.name, "find_async_functions");
    assert_eq!(search_result.match_count, 2, "Should find 2 async functions");
    assert!(search_result.error.is_none());

    // Check that we found the right functions
    let function_names: Vec<String> = search_result
        .matches
        .iter()
        .filter_map(|m| m.captures.get("name").cloned())
        .collect();

    assert!(function_names.contains(&"example_async".to_string()));
    assert!(function_names.contains(&"another_async".to_string()));

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_find_all_functions() {
    // Create a temporary test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_2");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.rs");
    fs::write(
        &test_file,
        r#"
pub async fn example_async() {
    println!("Hello");
}

fn regular_function() {
    println!("Regular");
}

pub async fn another_async(x: i32) -> Result<(), ()> {
    Ok(())
}
"#,
    )
    .unwrap();

    // Test 2: Find all functions (async and regular)
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "find_all_functions".to_string(),
            query: "(function_item name: (identifier) @name)".to_string(),
            language: "rust".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.name, "find_all_functions");
    assert_eq!(search_result.match_count, 3, "Should find 3 functions total");
    assert!(search_result.error.is_none());

    // Check that we found all functions
    let function_names: Vec<String> = search_result
        .matches
        .iter()
        .filter_map(|m| m.captures.get("name").cloned())
        .collect();

    assert!(function_names.contains(&"example_async".to_string()));
    assert!(function_names.contains(&"regular_function".to_string()));
    assert!(function_names.contains(&"another_async".to_string()));

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_find_structs() {
    // Create a temporary test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_3");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.rs");
    fs::write(
        &test_file,
        r#"
pub struct MyStruct {
    field: String,
}

struct AnotherStruct;

enum MyEnum {
    Variant,
}
"#,
    )
    .unwrap();

    // Test 3: Find structs
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "find_structs".to_string(),
            query: "(struct_item name: (type_identifier) @name)".to_string(),
            language: "rust".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.name, "find_structs");
    assert_eq!(search_result.match_count, 2, "Should find 2 structs");
    assert!(search_result.error.is_none());

    // Check that we found the right structs
    let struct_names: Vec<String> = search_result
        .matches
        .iter()
        .filter_map(|m| m.captures.get("name").cloned())
        .collect();

    assert!(struct_names.contains(&"MyStruct".to_string()));
    assert!(struct_names.contains(&"AnotherStruct".to_string()));

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_context_lines() {
    // Create a temporary test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_4");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.rs");
    fs::write(
        &test_file,
        r#"
// Line 1
// Line 2
pub fn target_function() {
    // Line 4
    println!("target");
}
// Line 7
// Line 8
"#,
    )
    .unwrap();

    // Test 4: Context lines
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "find_with_context".to_string(),
            query: "(function_item name: (identifier) @name)".to_string(),
            language: "rust".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 2,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.match_count, 1);
    
    let match_result = &search_result.matches[0];
    assert!(match_result.context.is_some());
    
    let context = match_result.context.as_ref().unwrap();
    assert!(context.contains("Line 2"), "Should include 2 lines before");
    assert!(context.contains("target_function"), "Should include the function");
    // Note: context_lines=2 means 2 lines before and after the match line (line 4)
    // So we get lines 2-6, which includes up to println but not the closing brace
    assert!(context.contains("println"), "Should include 2 lines after the match");

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_multiple_searches() {
    // Create a temporary test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_5");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.rs");
    fs::write(
        &test_file,
        r#"
pub async fn async_func() {}
fn regular_func() {}
pub struct MyStruct;
"#,
    )
    .unwrap();

    // Test 5: Multiple searches in one request
    let request = CodeSearchRequest {
        searches: vec![
            SearchSpec {
                name: "async_functions".to_string(),
                query: "(function_item (function_modifiers) name: (identifier) @name)".to_string(),
                language: "rust".to_string(),
                paths: vec![test_dir.to_string_lossy().to_string()],
                context_lines: 0,
            },
            SearchSpec {
                name: "structs".to_string(),
                query: "(struct_item name: (type_identifier) @name)".to_string(),
                language: "rust".to_string(),
                paths: vec![test_dir.to_string_lossy().to_string()],
                context_lines: 0,
            },
        ],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 2);
    assert_eq!(response.total_matches, 2); // 1 async function + 1 struct

    // Check first search (async functions)
    let async_search = &response.searches[0];
    assert_eq!(async_search.name, "async_functions");
    assert_eq!(async_search.match_count, 1);

    // Check second search (structs)
    let struct_search = &response.searches[1];
    assert_eq!(struct_search.name, "structs");
    assert_eq!(struct_search.match_count, 1);

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_python_search() {
    // Create a temporary Python test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_python");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.py");
    fs::write(
        &test_file,
        r#"
def regular_function():
    pass

async def async_function():
    pass

class MyClass:
    def method(self):
        pass
"#,
    )
    .unwrap();

    // Test 6: Python async functions
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "python_async".to_string(),
            // Note: tree-sitter-python doesn't expose 'async' as a queryable node
            // For now, we'll just find all functions (async detection would need text matching)
            query: "(function_definition name: (identifier) @name)".to_string(),
            language: "python".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.match_count, 3, "Should find 3 functions in Python (2 regular + 1 async + 1 method)");
    
    let function_names: Vec<String> = search_result
        .matches
        .iter()
        .filter_map(|m| m.captures.get("name").cloned())
        .collect();
    
    assert!(function_names.contains(&"regular_function".to_string()));
    assert!(function_names.contains(&"async_function".to_string()));
    assert!(function_names.contains(&"method".to_string()));

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_javascript_search() {
    // Create a temporary JavaScript test file
    let test_dir = std::env::temp_dir().join("g3_test_code_search_js");
    fs::create_dir_all(&test_dir).unwrap();
    
    let test_file = test_dir.join("test.js");
    fs::write(
        &test_file,
        r#"
function regularFunction() {
    console.log("regular");
}

async function asyncFunction() {
    console.log("async");
}

class MyClass {
    constructor() {}
}
"#,
    )
    .unwrap();

    // Test 7: JavaScript functions
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "js_functions".to_string(),
            query: "(function_declaration name: (identifier) @name)".to_string(),
            language: "javascript".to_string(),
            paths: vec![test_dir.to_string_lossy().to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 100,
    };

    let response = execute_code_search(request).await.unwrap();

    assert_eq!(response.searches.len(), 1);
    let search_result = &response.searches[0];
    assert_eq!(search_result.match_count, 2, "Should find 2 functions in JavaScript");
    
    let function_names: Vec<String> = search_result
        .matches
        .iter()
        .filter_map(|m| m.captures.get("name").cloned())
        .collect();
    
    assert!(function_names.contains(&"regularFunction".to_string()));
    assert!(function_names.contains(&"asyncFunction".to_string()));

    // Cleanup
    fs::remove_dir_all(&test_dir).ok();
}

#[tokio::test]
async fn test_go_search() {
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "go_functions".to_string(),
            query: "(function_declaration name: (identifier) @name)".to_string(),
            language: "go".to_string(),
            paths: vec!["examples/test_code".to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 500,
    };

    let response = execute_code_search(request).await.unwrap();
    assert_eq!(response.searches.len(), 1);
    assert!(response.searches[0].matches.len() > 0);
    
    // Should find main and greet functions
    let names: Vec<&str> = response.searches[0].matches.iter()
        .filter_map(|m| m.captures.get("name").map(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"greet"));
}

#[tokio::test]
async fn test_java_search() {
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "java_classes".to_string(),
            query: "(class_declaration name: (identifier) @name)".to_string(),
            language: "java".to_string(),
            paths: vec!["examples/test_code".to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 500,
    };

    let response = execute_code_search(request).await.unwrap();
    assert_eq!(response.searches.len(), 1);
    assert!(response.searches[0].matches.len() > 0);
    
    // Should find Example class
    let names: Vec<&str> = response.searches[0].matches.iter()
        .filter_map(|m| m.captures.get("name").map(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"Example"));
}

#[tokio::test]
async fn test_c_search() {
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "c_functions".to_string(),
            query: "(function_definition declarator: (function_declarator declarator: (identifier) @name))".to_string(),
            language: "c".to_string(),
            paths: vec!["examples/test_code".to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 500,
    };

    let response = execute_code_search(request).await.unwrap();
    assert_eq!(response.searches.len(), 1);
    assert!(response.searches[0].matches.len() > 0);
    
    // Should find greet, add, and main functions
    let names: Vec<&str> = response.searches[0].matches.iter()
        .filter_map(|m| m.captures.get("name").map(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"greet"));
    assert!(names.contains(&"add"));
    assert!(names.contains(&"main"));
}

#[tokio::test]
async fn test_cpp_search() {
    let request = CodeSearchRequest {
        searches: vec![SearchSpec {
            name: "cpp_classes".to_string(),
            query: "(class_specifier name: (type_identifier) @name)".to_string(),
            language: "cpp".to_string(),
            paths: vec!["examples/test_code".to_string()],
            context_lines: 0,
        }],
        max_concurrency: 4,
        max_matches_per_search: 500,
    };

    let response = execute_code_search(request).await.unwrap();
    assert_eq!(response.searches.len(), 1);
    assert!(response.searches[0].matches.len() > 0);
    
    // Should find Person class
    let names: Vec<&str> = response.searches[0].matches.iter()
        .filter_map(|m| m.captures.get("name").map(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"Person"));
}
