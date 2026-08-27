// ============================================================================
// NATIVE SYSTEM PROMPT
// Loaded from external markdown file for easy editing
// ============================================================================


/// Embedded fallback prompt (used when external file is not available)
const EMBEDDED_NATIVE_PROMPT: &str = include_str!("../../../prompts/system/native.md");

// ============================================================================
// NON-NATIVE SPECIFIC SECTIONS
// These are only used by providers without native tool calling
// ============================================================================

const NON_NATIVE_TOOL_FORMAT: &str = "\
# Tool Call Format

When you need to execute a tool, write ONLY the JSON tool call on a new line:

{\"tool\": \"tool_name\", \"args\": {\"param\": \"value\"}}

The tool will execute immediately and you'll receive the result (success or error) to continue with.

# Available Tools

Short description for providers without native calling specs:

- **shell**: Execute shell commands
  - Format: {\"tool\": \"shell\", \"args\": {\"command\": \"your_command_here\"}}
  - Example: {\"tool\": \"shell\", \"args\": {\"command\": \"ls ~/Downloads\"}}
  - Always use `rg` (ripgrep) instead of `grep` - it's faster and respects .gitignore

- **background_process**: Launch a long-running process in the background (e.g., game servers, dev servers)
  - Format: {\"tool\": \"background_process\", \"args\": {\"name\": \"unique_name\", \"command\": \"your_command\"}}
  - Example: {\"tool\": \"background_process\", \"args\": {\"name\": \"game_server\", \"command\": \"./run.sh\"}}
  - Returns PID and log file path. Use shell tool to read logs (`tail -100 <logfile>`), check status (`ps -p <pid>`), or stop (`kill <pid>`)
  - Note: Process runs independently; logs are captured to a file for later inspection

- **read_file**: Read the contents of a file (supports partial reads via start/end)
  - Format: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"path/to/file\", \"start\": 0, \"end\": 100}}
  - Example: {\"tool\": \"read_file\", \"args\": {\"file_path\": \"src/main.rs\"}}
  - Example (partial): {\"tool\": \"read_file\", \"args\": {\"file_path\": \"large.log\", \"start\": 0, \"end\": 1000}}

- **read_image**: Read an image file for visual analysis (PNG, JPEG, GIF, WebP)
  - Format: {\"tool\": \"read_image\", \"args\": {\"file_paths\": [\"path/to/image.png\"]}}
  - Example: {\"tool\": \"read_image\", \"args\": {\"file_paths\": [\"sprites/fairy.png\"]}}

- **write_file**: Write content to a file (creates or overwrites)
  - Format: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"path/to/file\", \"content\": \"file content\"}}
  - Example: {\"tool\": \"write_file\", \"args\": {\"file_path\": \"src/lib.rs\", \"content\": \"pub fn hello() {}\"}}

- **str_replace**: Replace text in a file using a diff
  - Format: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"path/to/file\", \"diff\": \"--- old\\n-old text\\n+++ new\\n+new text\"}}
  - Example: {\"tool\": \"str_replace\", \"args\": {\"file_path\": \"src/main.rs\", \"diff\": \"--- old\\n-old_code();\\n+++ new\\n+new_code();\"}}

- **plan_read**: Read the current Plan for this session
  - Format: {\"tool\": \"plan_read\", \"args\": {}}
  - Example: {\"tool\": \"plan_read\", \"args\": {}}

- **plan_write**: Create or update the Plan with YAML content
  - Format: {\"tool\": \"plan_write\", \"args\": {\"plan\": \"plan_id: my-plan\\nitems: [...]\"}}
  - Example (new plan): {\"tool\": \"plan_write\", \"args\": {\"plan\": \"plan_id: feature-x\\nitems:\\n  - id: I1\\n    description: Add feature\\n    state: todo\\n    touches: [src/lib.rs]\\n    checks:\\n      happy: {desc: Works, target: lib}\\n      negative:\\n        - {desc: Errors, target: lib}\\n      boundary:\\n        - {desc: Edge, target: lib}\"}}
  - Example (update): {\"tool\": \"plan_write\", \"args\": {\"plan\": \"plan_id: feature-x\\nitems:\\n  - id: I1\\n    state: done\\n    evidence: [src/lib.rs:42]\\n    notes: Implemented\"}}

- **plan_approve**: Approve the current plan revision (called by user)
  - Format: {\"tool\": \"plan_approve\", \"args\": {}}
  - Example: {\"tool\": \"plan_approve\", \"args\": {}}

- **code_search**: Syntax-aware code search using tree-sitter. Supports Rust, Python, JavaScript, TypeScript.
  - Format: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"label\", \"query\": \"tree-sitter query\", \"language\": \"rust|python|javascript|typescript\", \"paths\": [\"src/\"], \"context_lines\": 0}]}}
  - Find functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_functions\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"paths\": [\"src/\"]}]}}
  - Find async functions: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"find_async\", \"query\": \"(function_item (function_modifiers) name: (identifier) @name)\", \"language\": \"rust\"}]}}
  - Find structs: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - Multiple searches: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\"}, {\"name\": \"structs\", \"query\": \"(struct_item name: (type_identifier) @name)\", \"language\": \"rust\"}]}}
  - With context lines: {\"tool\": \"code_search\", \"args\": {\"searches\": [{\"name\": \"funcs\", \"query\": \"(function_item name: (identifier) @name)\", \"language\": \"rust\", \"context_lines\": 3}]}}

- **remember**: Save discovered code locations to workspace memory
  - Format: {\"tool\": \"remember\", \"args\": {\"notes\": \"markdown notes\"}}
  - Example: {\"tool\": \"remember\", \"args\": {\"notes\": \"### Feature Name\\n- `file.rs` [0..100] - `function_name()\"}}
  - Use at the END of your turn after discovering code locations via search tools";

const NON_NATIVE_INSTRUCTIONS: &str = "\
# Instructions

1. Analyze the request and break down into smaller tasks if appropriate
2. Execute ONE tool at a time. An exception exists for when you're writing files. See below.
3. STOP when the original request was satisfied
4. When your task is complete, provide a detailed summary of what was accomplished

IMPORTANT: If the user asks you to just respond with text (like \"just say hello\" or \"tell me about X\"), do NOT use tools. Simply respond with the requested text directly. Only use tools when you need to execute commands or complete tasks that require action.

Do not explain what you're going to do - just do it by calling the tools.

For reading files, prioritize use of code_search tool use with multiple search requests per call instead of read_file, if it makes sense.

Exception to using ONE tool at a time:
If all you're doing is WRITING files, and you don't need to do anything else between each step.
You can issue MULTIPLE write_file tool calls in a request, however you may ONLY make a SINGLE write_file call for any file in that request.
For example you may call:
[START OF REQUEST]
write_file(\"helper.rs\", \"...\")
write_file(\"file2.txt\", \"...\")
[DONE]

But NOT:
[START OF REQUEST]
write_file(\"helper.rs\", \"...\")
write_file(\"file2.txt\", \"...\")
write_file(\"helper.rs\", \"...\")
[DONE]";


// ============================================================================
// COMPOSED PROMPTS
// ============================================================================

use crate::skills::{Skill, generate_skills_prompt};
use crate::toolsets::generate_toolsets_prompt;

/// Markers delimiting the auto-memory directive in `prompts/system/native.md`.
///
/// WHY THE PROMPT IS EDITED AND NOT JUST THE REMINDER
/// --------------------------------------------------
/// `--no-auto-memory` used to gate only `send_auto_memory_reminder()` — the extra
/// end-of-turn LLM round trip. But the system prompt *also* carries a standing
/// "call `remember` at end of turn when you discover code locations worth noting"
/// directive, which survived the flag. An agent that had been told never to
/// auto-capture was still being instructed to capture, every single turn, by the
/// prompt underneath it. For butler that meant tier-1 memory filling up with
/// engineering notes (65% of a 12k budget) while the flag reported success.
///
/// One switch must govern both, so the flag means what its name says.
const AUTO_MEMORY_BEGIN: &str = "<!-- BEGIN AUTO-MEMORY -->";
const AUTO_MEMORY_END: &str = "<!-- END AUTO-MEMORY -->";

/// Strip the auto-memory directive from an assembled prompt.
///
/// Removes everything between the markers inclusive. The `remember` TOOL stays
/// registered either way: disabling the *nag* must not remove the *ability*, so
/// an agent (or a human asking directly) can still capture deliberately.
///
/// If the markers are absent — a hand-edited or older prompt file — the prompt is
/// returned unchanged rather than mangled. A silent no-op is the right failure
/// here; a partial strip would be worse than none.
pub fn strip_auto_memory_directive(prompt: &str) -> String {
    let (Some(start), Some(end)) = (prompt.find(AUTO_MEMORY_BEGIN), prompt.find(AUTO_MEMORY_END))
    else {
        return prompt.to_string();
    };
    if end < start {
        return prompt.to_string();
    }
    let mut out = String::with_capacity(prompt.len());
    out.push_str(&prompt[..start]);
    out.push_str(&prompt[end + AUTO_MEMORY_END.len()..]);
    out.trim_end().to_string()
}

/// Remove only the marker comments, keeping the directive they wrap.
///
/// Call this on the auto-memory-ENABLED path so the markers never reach the
/// model. They are bookkeeping for `strip_auto_memory_directive`, not content.
pub fn remove_auto_memory_markers(prompt: &str) -> String {
    prompt
        .lines()
        .filter(|l| {
            let t = l.trim();
            t != AUTO_MEMORY_BEGIN && t != AUTO_MEMORY_END
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply the auto-memory setting to an assembled system prompt.
///
/// This is the single place that decides whether the standing "call `remember`"
/// directive is present, so the prompt and `send_auto_memory_reminder()` cannot
/// drift apart again.
pub fn apply_auto_memory(prompt: &str, auto_memory: bool) -> String {
    if auto_memory {
        remove_auto_memory_markers(prompt)
    } else {
        strip_auto_memory_directive(prompt)
    }
}

/// System prompt for providers with native tool calling (Anthropic, OpenAI, etc.)
/// Uses include_str! to embed the prompt at compile time.
pub fn get_system_prompt_for_native() -> String {
    get_system_prompt_for_native_with_skills(&[])
}

/// System prompt for providers with native tool calling, with skills support.
pub fn get_system_prompt_for_native_with_skills(skills: &[Skill]) -> String {
    let skills_section = generate_skills_prompt(skills);
    let toolsets_section = generate_toolsets_prompt();
    
    let mut prompt = EMBEDDED_NATIVE_PROMPT.to_string();
    
    // Add toolsets section (available toolsets for dynamic loading)
    if !toolsets_section.is_empty() {
        prompt = format!("{}\n\n{}", prompt, toolsets_section);
    }
    
    // Add skills section
    if !skills_section.is_empty() {
        prompt = format!("{}\n\n{}", prompt, skills_section);
    }
    
    prompt
}

/// System prompt for providers without native tool calling (embedded models)
pub fn get_system_prompt_for_non_native() -> String {
    get_system_prompt_for_non_native_with_skills(&[])
}

/// System prompt for providers without native tool calling, with skills support.
pub fn get_system_prompt_for_non_native_with_skills(skills: &[Skill]) -> String {
    // For non-native, we still need to inject the tool format instructions
    // We take the native prompt and insert the non-native sections after the intro
    let native = get_system_prompt_for_native_with_skills(skills);
    
    // Find the end of the intro section (after the first major heading)
    // The intro ends before "# Task Management with Plan Mode"
    if let Some(plan_section_start) = native.find("# Task Management with Plan Mode") {
        let intro = &native[..plan_section_start];
        let rest = &native[plan_section_start..];
        
        format!(
            "{}\n{}\n\n{}\n\n{}",
            intro.trim_end(),
            NON_NATIVE_TOOL_FORMAT,
            NON_NATIVE_INSTRUCTIONS,
            rest
        )
    } else {
        // Fallback: just prepend the non-native sections
        format!(
            "{}\n\n{}\n\n{}",
            native,
            NON_NATIVE_TOOL_FORMAT,
            NON_NATIVE_INSTRUCTIONS
        )
    }
}

/// The G3 identity line that gets replaced in agent mode
const G3_IDENTITY_LINE: &str = "You are G3, an AI programming agent.";

/// Generate a system prompt for agent mode by combining the agent's custom prompt
/// with the full G3 system prompt (including plan tools, code search, webdriver, coding style, etc.)
///
/// The agent_prompt replaces only the G3 identity line at the start of the prompt.
/// Everything else (tool instructions, coding guidelines, etc.) is preserved.
pub fn get_agent_system_prompt(agent_prompt: &str, allow_multiple_tool_calls: bool) -> String {
    get_agent_system_prompt_with_skills(agent_prompt, allow_multiple_tool_calls, &[])
}

/// Generate a system prompt for agent mode with skills support.
///
/// The agent_prompt replaces only the G3 identity line at the start of the prompt.
/// Everything else (tool instructions, coding guidelines, skills, etc.) is preserved.
pub fn get_agent_system_prompt_with_skills(agent_prompt: &str, allow_multiple_tool_calls: bool, skills: &[Skill]) -> String {
    // Get the full system prompt (always allows multiple tool calls now)
    let _ = allow_multiple_tool_calls; // Parameter kept for API compatibility but ignored
    let full_prompt = get_system_prompt_for_native_with_skills(skills);

    // Replace only the G3 identity line with the custom agent prompt
    full_prompt.replace(G3_IDENTITY_LINE, agent_prompt.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_prompt_contains_validation_string() {
        let prompt = get_system_prompt_for_native();
        assert!(prompt.contains("Use tools to accomplish tasks"),
            "Native prompt must contain tool usage instruction");
    }

    #[test]
    fn test_non_native_prompt_contains_validation_string() {
        let prompt = get_system_prompt_for_non_native();
        assert!(prompt.contains("Use tools to accomplish tasks"),
            "Non-native prompt must contain tool usage instruction");
    }

    #[test]
    fn test_native_prompt_contains_important_directive() {
        let prompt = get_system_prompt_for_native();
        assert!(prompt.contains("# Task Management with Plan Mode"),
            "Native prompt must contain Plan Mode section");
    }

    #[test]
    fn test_non_native_prompt_contains_important_directive() {
        let prompt = get_system_prompt_for_non_native();
        assert!(prompt.contains("# Task Management with Plan Mode"),
            "Non-native prompt must contain Plan Mode section");
    }

    #[test]
    fn test_non_native_prompt_contains_tool_format() {
        let prompt = get_system_prompt_for_non_native();
        assert!(prompt.contains("# Tool Call Format"),
            "Non-native prompt must contain tool format section");
        assert!(prompt.contains("# Available Tools"),
            "Non-native prompt must contain available tools section");
    }

    #[test]
    fn test_agent_prompt_replaces_identity() {
        let custom = "You are TestAgent, a specialized testing assistant.";
        let prompt = get_agent_system_prompt(custom, true);
        assert!(prompt.contains(custom), "Agent prompt should contain custom identity");
        assert!(!prompt.contains(G3_IDENTITY_LINE), "Agent prompt should not contain G3 identity");
    }

    #[test]
    fn test_both_prompts_have_plan_section() {
        let native = get_system_prompt_for_native();
        let non_native = get_system_prompt_for_non_native();
        
        assert!(native.contains("# Task Management with Plan Mode"));
        assert!(non_native.contains("# Task Management with Plan Mode"));
    }

    #[test]
    fn test_both_prompts_have_workspace_memory() {
        let native = get_system_prompt_for_native();
        let non_native = get_system_prompt_for_non_native();
        
        assert!(native.contains("# Workspace Memory"));
        assert!(non_native.contains("# Workspace Memory"));
    }

    #[test]
    fn test_native_prompt_loaded_from_file() {
        // Verify the include_str! macro successfully loads the file
        let prompt = EMBEDDED_NATIVE_PROMPT;
        assert!(!prompt.is_empty(), "Embedded prompt should not be empty");
        assert!(prompt.starts_with("You are G3"), "Prompt should start with agent introduction");
    }

    #[test]
    fn test_native_prompt_without_skills() {
        let prompt = get_system_prompt_for_native_with_skills(&[]);
        assert!(!prompt.contains("<available_skills>"));
        assert!(!prompt.contains("# Available Skills"));
    }

    #[test]
    fn test_native_prompt_with_skills() {
        let skills = vec![Skill {
            name: "test-skill".to_string(),
            description: "A test skill for unit testing".to_string(),
            license: None,
            compatibility: None,
            metadata: None,
            allowed_tools: None,
            body: String::new(),
            path: "/path/to/test-skill/SKILL.md".to_string(),
        }];
        
        let prompt = get_system_prompt_for_native_with_skills(&skills);
        assert!(prompt.contains("# Available Skills"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test-skill</name>"));
        assert!(prompt.contains("<description>A test skill for unit testing</description>"));
        assert!(prompt.contains("<location>/path/to/test-skill/SKILL.md</location>"));
    }

    #[test]
    fn test_agent_prompt_with_skills() {
        let skills = vec![Skill {
            name: "agent-skill".to_string(),
            description: "Skill for agent mode".to_string(),
            license: None,
            compatibility: None,
            metadata: None,
            allowed_tools: None,
            body: String::new(),
            path: "/path/to/SKILL.md".to_string(),
        }];
        
        let prompt = get_agent_system_prompt_with_skills("Custom agent", true, &skills);
        assert!(prompt.contains("Custom agent"));
        assert!(prompt.contains("<name>agent-skill</name>"));
    }

    // ================================================================
    // AUTO-MEMORY DIRECTIVE GATING
    //
    // `--no-auto-memory` gated only the end-of-turn reminder turn; the standing
    // "call `remember`" directive in the system prompt survived it. butler ran
    // with the flag set for months and still filled 65% of its 12k tier-1 memory
    // budget with engineering notes, because the prompt kept asking.
    // ================================================================

    /// HAPPY: with auto-memory off, no directive to call `remember` survives.
    #[test]
    fn test_auto_memory_directive_stripped_when_disabled() {
        let prompt = apply_auto_memory(&get_system_prompt_for_native(), false);
        assert!(
            !prompt.contains("Call `remember` at end of turn"),
            "the standing remember directive must be gone when auto-memory is off"
        );
        assert!(
            !prompt.contains("# Workspace Memory"),
            "the whole Workspace Memory section is the directive; it must go too"
        );
    }

    /// NEGATIVE: the flag must not silently disable auto-memory for everyone.
    /// This is the mutation that matters — a strip applied unconditionally would
    /// pass the happy test above and break every other g3 agent.
    #[test]
    fn test_auto_memory_directive_retained_when_enabled() {
        let prompt = apply_auto_memory(&get_system_prompt_for_native(), true);
        assert!(
            prompt.contains("Call `remember` at end of turn"),
            "default (auto-memory on) must still instruct the model to remember"
        );
        assert!(prompt.contains("# Workspace Memory"));
    }

    /// BOUNDARY: the marker comments are bookkeeping and must never reach the
    /// model on EITHER path, or the prompt leaks HTML comments at the model.
    #[test]
    fn test_auto_memory_markers_never_reach_the_model() {
        for enabled in [true, false] {
            let prompt = apply_auto_memory(&get_system_prompt_for_native(), enabled);
            assert!(
                !prompt.contains("BEGIN AUTO-MEMORY") && !prompt.contains("END AUTO-MEMORY"),
                "marker leaked with auto_memory={}",
                enabled
            );
        }
    }

    /// BOUNDARY: a prompt without markers (hand-edited, or older file) must be
    /// returned unchanged rather than truncated. A partial strip is worse than none.
    #[test]
    fn test_strip_is_noop_without_markers() {
        let plain = "# Some Prompt\n\nNo markers here.";
        assert_eq!(strip_auto_memory_directive(plain), plain);
        assert_eq!(remove_auto_memory_markers(plain), plain);
    }

    /// BOUNDARY: markers in the wrong order must not produce a garbled slice.
    #[test]
    fn test_strip_is_noop_when_markers_inverted() {
        let inverted = format!("head {} middle {} tail", AUTO_MEMORY_END, AUTO_MEMORY_BEGIN);
        assert_eq!(strip_auto_memory_directive(&inverted), inverted);
    }

    /// BOUNDARY: disabling the NAG must not remove the ABILITY. The `remember`
    /// tool stays registered so deliberate capture still works.
    #[test]
    fn test_agent_prompt_still_usable_with_auto_memory_off() {
        let prompt = apply_auto_memory(
            &get_agent_system_prompt("You are butler.", true),
            false,
        );
        assert!(prompt.contains("You are butler."));
        assert!(
            prompt.contains("Use tools to accomplish tasks"),
            "stripping the directive must not damage the rest of the prompt"
        );
    }
}
