//! Tool definitions for the agent's available tools.
//!
//! This module contains the JSON schema definitions for all tools that can be
//! used by the agent when interacting with LLM providers that support native
//! tool calling.

use g3_providers::Tool;
use serde_json::json;

/// Kept as a zero-sized marker so call sites and tests keep their shape;
/// the only optional set it ever governed (desktop control) was removed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolConfig;

impl ToolConfig {
    pub fn new() -> Self {
        Self
    }
}

/// Create tool definitions for native tool calling providers.
///
/// Returns a vector of Tool definitions that describe the available tools
/// and their input schemas.
pub fn create_tool_definitions(_config: ToolConfig) -> Vec<Tool> {
    // Webdriver tools are now JIT-loaded via load_toolset("webdriver")
    create_core_tools()
}

/// Create the core tools that are always available
fn create_core_tools() -> Vec<Tool> {
    let mut tools = vec![
        Tool {
            name: "shell".to_string(),
            description: "Execute shell commands in the current working directory. Do NOT prefix commands with `cd <path> &&` - commands already run in the workspace. Several shell/read_file/read_image/code_search calls in ONE response run CONCURRENTLY — batch independent reads and searches together, and never put a call in the same batch as one it depends on.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }),
        },
        Tool {
            name: "background_process".to_string(),
            description: "Launch a long-running process in the background (e.g., game servers, dev servers). The process runs independently and logs are captured to a file. Use the regular 'shell' tool to read logs (cat/tail), check status (ps), or stop the process (kill). Returns the PID and log file path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "A unique name for this process (e.g., 'game_server', 'my_app'). Used to identify the process and its log file."
                    },
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute in the background"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory. Defaults to current directory if not specified."
                    }
                },
                "required": ["name", "command"]
            }),
        },
        Tool {
            name: "read_file".to_string(),
            description: "Read the contents of a file. Optionally read a specific character range.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to read"
                    },
                    "start": {
                        "type": "integer",
                        "description": "Starting character position (0-indexed, inclusive). If omitted, reads from beginning."
                    },
                    "end": {
                        "type": "integer",
                        "description": "Ending character position (0-indexed, EXCLUSIVE). If omitted, reads to end of file."
                    }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "read_image".to_string(),
            description: "Read one or more image files and send them to the LLM for visual analysis. Supports PNG, JPEG, GIF, and WebP formats. Use this when you need to visually inspect images (e.g., find sprites, analyze UI, read diagrams). The images will be included in your next response for analysis.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of paths to image files to read"
                    }
                },
                "required": ["file_paths"]
            }),
        },
        Tool {
            name: "write_file".to_string(),
            description: "Write content to a file (creates or overwrites). You MUST provide all arguments".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                },
                "required": ["file_path", "content"]
            }),
        },
        Tool {
            name: "str_replace".to_string(),
            description: "Apply a unified diff to a file. Supports multiple hunks and context lines. Optionally constrain the search to a [start, end) character range (0-indexed; end is EXCLUSIVE). Useful to disambiguate matches or limit scope in large files.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to edit"
                    },
                    "diff": {
                        "type": "string",
                        "description": "A unified diff showing what to replace. Supports @@ hunk headers, context lines, and multiple hunks (---/+++ headers optional for minimal diffs)."
                    },
                    "start": {
                        "type": "integer",
                        "description": "Starting character position in the file (0-indexed, inclusive). If omitted, searches from beginning."
                    },
                    "end": {
                        "type": "integer",
                        "description": "Ending character position in the file (0-indexed, EXCLUSIVE - character at this position is NOT included). If omitted, searches to end of file."
                    }
                },
                "required": ["file_path", "diff"]
            }),
        },
        Tool {
            name: "code_search".to_string(),
            description: "Syntax-aware code search that understands code structure, not just text. Finds actual functions, classes, methods, and other code constructs - ignores matches in comments and strings. Much more accurate than grep for code searches. Supports batch searches (up to 20 parallel) with structured results and context lines. Languages: Rust, Python, JavaScript, TypeScript, Go, Java, C, C++, Racket. Uses tree-sitter query syntax.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "searches": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "Label for this search." },
                                "query": { "type": "string", "description": "tree-sitter query in S-expression format (e.g., \"(function_item name: (identifier) @name)\")" },
                                "language": { "type": "string", "enum": ["rust", "python", "javascript", "typescript", "go", "java", "c", "cpp", "racket"], "description": "Programming language to search." },
                                "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths/dirs to search. Defaults to current dir if empty." },
                                "context_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 0, "description": "Lines of context to include around each match." }
                            },
                            "required": ["name", "query", "language"]
                        }
                    },
                    "max_concurrency": { "type": "integer", "minimum": 1, "default": 4 },
                    "max_matches_per_search": { "type": "integer", "minimum": 1, "default": 500 }
                },
                "required": ["searches"]
            }),
        },
    ];

    // Action Envelope tool — the deterministic gate for OUTBOUND actions
    // (e.g. butler's email sender verifies its `g3v1:` token). Not part of
    // planning: code-change envelopes verified nothing (2026-11-26).
    tools.push(Tool {
        name: "write_envelope".to_string(),
        description: "Write an action envelope for an outbound action (e.g. sending an email) and verify it against analysis/rulespec.yaml. Only call this when a workflow requires an envelope; on success it is stamped with a verification token the downstream sender checks.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "facts": {
                    "type": "string",
                    "description": "The envelope facts as YAML. MUST have a top-level `facts:` key containing all fact groups. No other top-level keys are allowed except envelope metadata (e.g. `type:`). Each fact group is a named map under `facts:`; the workflow that asks for the envelope defines its fields.\n\nExample:\n\nfacts:\n  email:\n    to: [\"a@example.com\"]\n    subject: \"Hello\"\n    reviewed: true"
                }
            },
            "required": ["facts"]
        }),
    });

    tools.push(Tool {
        name: "plan_write".to_string(),
        description: "Record or update the checklist for multi-step work, AFTER the user has approved it. Send the whole plan each time; g3 keeps count. An unfinished plan cannot drop items — mark them blocked instead.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "YAML: `plan_id` and `items`, each item `{id, description, state}` with state todo|doing|done|blocked and an optional one-line `notes`.\n\nExample:\n\nplan_id: csv-import\nitems:\n  - {id: I1, description: Parse CSV with headers, state: done}\n  - {id: I2, description: Reject rows with missing columns, state: doing}\n  - {id: I3, description: Wire into the importer UI, state: blocked, notes: waiting on design}"
                }
            },
            "required": ["plan"]
        }),
    });

    // Workspace memory tool (memory is auto-loaded at startup, only remember is needed)
    tools.push(Tool {
        name: "remember".to_string(),
        description: "Update the workspace memory with new discoveries. Call this at the END of your turn (before your summary) if you discovered something worth noting. Provide your notes in markdown format - they will be merged with existing memory.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "notes": {
                    "type": "string",
                    "description": "New discoveries to add to memory in markdown format. Use the format:\n### Feature Name\n- `file/path.rs` [start..end] - `function_name()`, `StructName`\n\nDo not include content already in memory."
                }
            },
            "required": ["notes"]
        }),
    });

    // Toolset loading tool - allows dynamic loading of additional tools
    tools.push(Tool {
        name: "load_toolset".to_string(),
        description: "Load additional tools from a named toolset. Returns the tool definitions so you learn how to use them. Use this when you need specialized tools like browser automation.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the toolset to load (e.g., 'webdriver')"
                }
            },
            "required": ["name"]
        }),
    });

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_tools_count() {
        let tools = create_core_tools();
        // Core tools: shell, background_process, read_file, read_image,
        // write_file, str_replace, code_search, plan_write,
        // remember, load_toolset, write_envelope
        // (11 total — plan_read/plan_approve removed 2026-11-26; research tools are in a loadable toolset)
        assert_eq!(tools.len(), 11);
    }

    #[test]
    fn test_create_tool_definitions_core_only() {
        let config = ToolConfig::default();
        let tools = create_tool_definitions(config);
        // 11 core tools (webdriver and research are now JIT-loaded)
        assert_eq!(tools.len(), 11);
    }

    #[test]
    fn test_create_tool_definitions() {
        let config = ToolConfig::new();
        let tools = create_tool_definitions(config);
        // Webdriver and research tools are now JIT-loaded, so only core tools are included
        assert_eq!(tools.len(), 11);
    }

    #[test]
    fn test_removed_tools_stay_removed() {
        let names: Vec<String> = create_core_tools().into_iter().map(|t| t.name).collect();
        for gone in ["rehydrate", "take_screenshot", "code_coverage", "plan_read", "plan_approve"] {
            assert!(!names.iter().any(|n| n == gone), "{gone} is back in the tool list");
        }
    }

    #[test]
    fn test_tool_has_required_fields() {
        let tools = create_core_tools();
        for tool in tools {
            assert!(!tool.name.is_empty(), "Tool name should not be empty");
            assert!(!tool.description.is_empty(), "Tool description should not be empty");
            assert!(tool.input_schema.is_object(), "Tool input_schema should be an object");
        }
    }
}
