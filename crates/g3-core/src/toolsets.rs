//! Toolsets module - registry of dynamically loadable tool collections.
//!
//! Toolsets are groups of related tools that can be loaded on-demand via the
//! `load_toolset` tool. This keeps the default tool set lean while allowing
//! access to specialized tools when needed.
//!
//! The agent sees a concise registry in the system prompt and can load
//! toolsets as needed. The tool definitions are returned so the agent
//! learns how to call the newly available tools.

use g3_providers::Tool;
use serde_json::json;

/// A toolset that can be dynamically loaded.
#[derive(Debug, Clone)]
pub struct Toolset {
    /// Unique identifier for the toolset (e.g., "webdriver")
    pub name: &'static str,
    /// Brief description of what the toolset provides
    pub description: &'static str,
    /// Function that returns the tool definitions for this toolset
    tool_definitions_fn: fn() -> Vec<Tool>,
}

impl Toolset {
    /// Get the tool definitions for this toolset.
    pub fn get_tools(&self) -> Vec<Tool> {
        (self.tool_definitions_fn)()
    }
}

/// Registry of all available toolsets.
/// Add new toolsets here as they are created.
const TOOLSET_REGISTRY: &[Toolset] = &[
    Toolset {
        name: "webdriver",
        description: "Browser automation via Safari WebDriver. Start sessions, navigate, find elements, click, type, execute JavaScript, take screenshots.",
        tool_definitions_fn: create_webdriver_tools,
    },
    Toolset {
        name: "research",
        description: "Web research in seconds: ask a question, get a concise answer with cited sources (server-side web search + page fetch, no browser). Also a slow browser-driven scout for logged-in or interactive pages.",
        tool_definitions_fn: create_research_tools,
    },
];

/// Get a toolset by name.
///
/// Returns `Ok(Toolset)` if found, or `Err` with a helpful message listing
/// available toolsets if not found.
pub fn get_toolset(name: &str) -> Result<&'static Toolset, String> {
    let name = name.trim();
    
    if name.is_empty() {
        return Err(format!(
            "Toolset name cannot be empty. Available toolsets: {}",
            list_toolset_names().join(", ")
        ));
    }
    
    TOOLSET_REGISTRY
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| {
            format!(
                "Unknown toolset '{}'. Available toolsets: {}",
                name,
                list_toolset_names().join(", ")
            )
        })
}

/// List all available toolset names.
pub fn list_toolset_names() -> Vec<&'static str> {
    TOOLSET_REGISTRY.iter().map(|t| t.name).collect()
}

/// Get all available toolsets.
pub fn get_all_toolsets() -> &'static [Toolset] {
    TOOLSET_REGISTRY
}

/// Generate the prompt section describing available toolsets.
///
/// This is injected into the system prompt so the agent knows what
/// toolsets are available to load.
pub fn generate_toolsets_prompt() -> String {
    if TOOLSET_REGISTRY.is_empty() {
        return String::new();
    }
    
    let mut prompt = String::new();
    prompt.push_str("# Available Toolsets\n\n");
    prompt.push_str("You can dynamically load additional tools using `load_toolset`. ");
    prompt.push_str("The tool will return the full definitions so you learn how to use them.\n\n");
    prompt.push_str("<available_toolsets>\n");
    
    for toolset in TOOLSET_REGISTRY {
        prompt.push_str("  <toolset>\n");
        prompt.push_str(&format!("    <name>{}</name>\n", escape_xml(toolset.name)));
        prompt.push_str(&format!("    <description>{}</description>\n", escape_xml(toolset.description)));
        prompt.push_str("  </toolset>\n");
    }
    
    prompt.push_str("</available_toolsets>\n");
    prompt
}

/// Escape special XML characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// =============================================================================
// TOOLSET DEFINITIONS
// =============================================================================

/// Create WebDriver browser automation tools.
///
/// These tools enable browser automation via Safari WebDriver.
fn create_webdriver_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "webdriver_start".to_string(),
            description: "Start a Safari WebDriver session for browser automation. Must be called before any other webdriver tools. Requires Safari's 'Allow Remote Automation' to be enabled in Develop menu.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_navigate".to_string(),
            description: "Navigate to a URL in the browser".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to navigate to (must include protocol, e.g., https://)"
                    }
                },
                "required": ["url"]
            }),
        },
        Tool {
            name: "webdriver_get_url".to_string(),
            description: "Get the current URL of the browser".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_get_title".to_string(),
            description: "Get the title of the current page".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_find_element".to_string(),
            description: "Find an element on the page by CSS selector and return its text content".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector to find the element (e.g., 'h1', '.class-name', '#id')"
                    }
                },
                "required": ["selector"]
            }),
        },
        Tool {
            name: "webdriver_find_elements".to_string(),
            description: "Find all elements matching a CSS selector and return their text content".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector to find elements"
                    }
                },
                "required": ["selector"]
            }),
        },
        Tool {
            name: "webdriver_click".to_string(),
            description: "Click an element on the page".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the element to click"
                    }
                },
                "required": ["selector"]
            }),
        },
        Tool {
            name: "webdriver_send_keys".to_string(),
            description: "Type text into an input element".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the input element"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type into the element"
                    },
                    "clear_first": {
                        "type": "boolean",
                        "description": "Whether to clear the element before typing (default: true)"
                    }
                },
                "required": ["selector", "text"]
            }),
        },
        Tool {
            name: "webdriver_execute_script".to_string(),
            description: "Execute JavaScript code in the browser and return the result".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "script": {
                        "type": "string",
                        "description": "JavaScript code to execute (use 'return' to return a value)"
                    }
                },
                "required": ["script"]
            }),
        },
        Tool {
            name: "webdriver_get_page_source".to_string(),
            description: "Get the rendered HTML source of the current page. Returns the current DOM state after JavaScript execution.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "max_length": {
                        "type": "integer",
                        "description": "Maximum length of HTML to return (default: 10000, use 0 for no truncation)"
                    },
                    "save_to_file": {
                        "type": "string",
                        "description": "Optional file path to save the HTML instead of returning it inline"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "webdriver_screenshot".to_string(),
            description: "Take a screenshot of the browser window".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path where to save the screenshot (e.g., '/tmp/screenshot.png')"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "webdriver_back".to_string(),
            description: "Navigate back in browser history".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_forward".to_string(),
            description: "Navigate forward in browser history".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_refresh".to_string(),
            description: "Refresh the current page".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "webdriver_quit".to_string(),
            description: "Close the browser and end the WebDriver session".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    ]
}

/// Create research tools.
///
/// `research` answers synchronously via server-side web search by default
/// (`tools/web_research.rs`); `browser: true` takes the async scout path,
/// which `research_status` reports on.
fn create_research_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "research".to_string(),
            description: "Answer a question from the web in seconds (typically 5-60s): Anthropic's server-side web search and page fetch read the sources and return a concise answer with a numbered, cited source list and a cost line. Synchronous - the answer is the tool result. Use it for APIs, SDKs, libraries, docs, versions, bugs, prices, rules, current events. Several research calls in one response run in parallel. Treat the answer as a lead: verify load-bearing claims against the cited sources. Cost: ~5-20c quick, ~50c-$1.50 deep. Set browser=true ONLY for logged-in or interactive pages, or when web search fell short: that launches a slow (~10 min) asynchronous browser scout that returns a research_id; its report is injected later (check with research_status) and ending the turn kills it.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The question. Be specific about what you need to know, and include any context (locale, version, platform) that changes the answer."
                    },
                    "deep": {
                        "type": "boolean",
                        "description": "Multi-angle or nuanced question (comparisons, legal/medical, conflicting sources): stronger model, up to 15 searches, longer answer. Slower and ~10x the cost. Default false."
                    },
                    "url": {
                        "type": "string",
                        "description": "Optional: read this specific page and answer the query from it."
                    },
                    "domains": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional: restrict search to these domains (e.g. [\"docs.rs\", \"github.com\"]). Useful when results are SEO noise and the official site is missing."
                    },
                    "browser": {
                        "type": "boolean",
                        "description": "Use the slow asynchronous browser scout instead of web search. Default false."
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "research_status".to_string(),
            description: "Check the status of pending browser research (research with browser=true). Call without arguments to list all pending research, or with a research_id to check a specific task.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "research_id": {
                        "type": "string",
                        "description": "Optional: specific research_id to check. If omitted, lists all pending research tasks."
                    }
                },
                "required": []
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_toolset_webdriver() {
        let toolset = get_toolset("webdriver").unwrap();
        assert_eq!(toolset.name, "webdriver");
        assert!(!toolset.description.is_empty());
        
        let tools = toolset.get_tools();
        assert!(!tools.is_empty());
        assert!(tools.iter().any(|t| t.name == "webdriver_start"));
    }

    #[test]
    fn test_get_toolset_unknown() {
        let result = get_toolset("nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Unknown toolset"));
        assert!(err.contains("webdriver")); // Should list available toolsets
    }

    #[test]
    fn test_get_toolset_empty_name() {
        let result = get_toolset("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("cannot be empty"));
    }

    #[test]
    fn test_get_toolset_whitespace_trimmed() {
        let toolset = get_toolset("  webdriver  ").unwrap();
        assert_eq!(toolset.name, "webdriver");
    }

    #[test]
    fn test_list_toolset_names() {
        let names = list_toolset_names();
        assert!(names.contains(&"webdriver"));
    }

    #[test]
    fn test_generate_toolsets_prompt() {
        let prompt = generate_toolsets_prompt();
        assert!(prompt.contains("<available_toolsets>"));
        assert!(prompt.contains("</available_toolsets>"));
        assert!(prompt.contains("<name>webdriver</name>"));
        assert!(prompt.contains("load_toolset"));
    }

    #[test]
    fn test_xml_escaping() {
        // The current toolsets don't have special chars, but test the function
        assert_eq!(escape_xml("<test>"), "&lt;test&gt;");
        assert_eq!(escape_xml("a & b"), "a &amp; b");
    }

    #[test]
    fn test_webdriver_tools_complete() {
        let tools = create_webdriver_tools();
        let tool_names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        
        // Verify all expected webdriver tools are present
        assert!(tool_names.contains(&"webdriver_start"));
        assert!(tool_names.contains(&"webdriver_navigate"));
        assert!(tool_names.contains(&"webdriver_get_url"));
        assert!(tool_names.contains(&"webdriver_get_title"));
        assert!(tool_names.contains(&"webdriver_find_element"));
        assert!(tool_names.contains(&"webdriver_find_elements"));
        assert!(tool_names.contains(&"webdriver_click"));
        assert!(tool_names.contains(&"webdriver_send_keys"));
        assert!(tool_names.contains(&"webdriver_execute_script"));
        assert!(tool_names.contains(&"webdriver_get_page_source"));
        assert!(tool_names.contains(&"webdriver_screenshot"));
        assert!(tool_names.contains(&"webdriver_back"));
        assert!(tool_names.contains(&"webdriver_forward"));
        assert!(tool_names.contains(&"webdriver_refresh"));
        assert!(tool_names.contains(&"webdriver_quit"));
    }

    #[test]
    fn test_get_toolset_research() {
        let toolset = get_toolset("research").unwrap();
        assert_eq!(toolset.name, "research");
        assert!(!toolset.description.is_empty());
        
        let tools = toolset.get_tools();
        assert_eq!(tools.len(), 2); // research and research_status
        assert!(tools.iter().any(|t| t.name == "research"));
        assert!(tools.iter().any(|t| t.name == "research_status"));
    }

    #[test]
    fn test_research_tools_complete() {
        let tools = create_research_tools();
        assert_eq!(tools.len(), 2);
        
        let tool_names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"research"));
        assert!(tool_names.contains(&"research_status"));
    }

    #[test]
    fn test_research_schema_exposes_web_options() {
        let tools = create_research_tools();
        let r = tools.iter().find(|t| t.name == "research").unwrap();
        let props = r.input_schema["properties"].as_object().unwrap();
        for k in ["query", "deep", "url", "domains", "browser"] {
            assert!(props.contains_key(k), "research schema missing '{}'", k);
        }
        assert_eq!(r.input_schema["required"], json!(["query"]));
    }

    #[test]
    fn test_list_toolset_names_includes_research() {
        let names = list_toolset_names();
        assert!(names.contains(&"research"));
        assert!(names.contains(&"webdriver"));
    }
}
