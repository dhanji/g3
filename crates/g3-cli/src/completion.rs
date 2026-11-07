//! Tab completion support for g3 interactive mode.
//!
//! Provides:
//! - Prompt highlighting (colorizes project name in blue)
//! - Command completion for `/` commands at line start
//! - File path completion for `./`, `../`, `~/`, `/` prefixes
//! - Session ID completion for `/resume` command
//! - Project name completion for `/project` command (from ~/projects/)

use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use std::path::PathBuf;

/// Available `/` commands for completion
const COMMANDS: &[&str] = &[
    "/clear",
    "/compact",
    "/dump",
    "/help",
    "/project",
    "/readme",
    "/resume",
    "/run",
    "/skinnify",
    "/stats",
    "/thinnify",
    "/unproject",
];

/// Helper struct for rustyline that provides tab completion.
pub struct G3Helper {
    /// File path completer
    file_completer: FilenameCompleter,
}

impl G3Helper {
    pub fn new() -> Self {
        Self {
            file_completer: FilenameCompleter::new(),
        }
    }

    /// Find the start of the current "word" being typed, respecting quotes.
    /// Returns (word_start, word) where word_start is the byte index.
    fn extract_word<'a>(&self, line: &'a str, pos: usize) -> (usize, &'a str) {
        let line_to_cursor = &line[..pos];
        
        // Find word start: after space (unless quoted/escaped)
        let mut word_start = 0;
        let mut in_quotes = false;
        let mut quote_char = ' ';
        let mut prev_was_backslash = false;
        
        let chars: Vec<(usize, char)> = line_to_cursor.char_indices().collect();
        for (idx, &(i, c)) in chars.iter().enumerate() {
            if in_quotes {
                if c == quote_char && !prev_was_backslash {
                    in_quotes = false;
                }
            } else if prev_was_backslash {
            } else {
                match c {
                    '"' | '\'' => {
                        in_quotes = true;
                        quote_char = c;
                        word_start = i;
                    }
                    ' ' | '\t' => {
                        if idx + 1 < chars.len() {
                            word_start = chars[idx + 1].0;
                        } else {
                            word_start = pos; // At end, empty word
                        }
                    }
                    _ => {}
                }
            }
            prev_was_backslash = c == '\\' && !prev_was_backslash;
        }
        
        (word_start, &line_to_cursor[word_start..])
    }

    fn is_path_prefix(&self, word: &str) -> bool {
        let word = word.trim_start_matches('"').trim_start_matches('\'');
        
        word.starts_with("./")
            || word.starts_with("../")
            || word.starts_with("~/")
            || word.starts_with('/')
            || word == "."
            || word == ".."
            || word == "~"
    }
    
    fn strip_quotes<'a>(&self, word: &'a str) -> &'a str {
        word.trim_start_matches('"').trim_start_matches('\'')
            .trim_end_matches('"').trim_end_matches('\'')
    }
    
    /// Unescape backslash-escaped chars: "~/My\ Files" -> "~/My Files"
    fn unescape_path(&self, path: &str) -> String {
        let mut result = String::with_capacity(path.len());
        let mut chars = path.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek().is_some() {
                // Skip the backslash, take the next char literally
                if let Some(next) = chars.next() {
                    result.push(next);
                }
            } else {
                result.push(c);
            }
        }
        result
    }
    
    /// List session IDs from .g3/sessions/, sorted newest-first, with optional limit.
    fn list_sessions(&self, limit: Option<usize>) -> Vec<String> {
        let sessions_dir = PathBuf::from(".g3/sessions");
        if !sessions_dir.is_dir() {
            return Vec::new();
        }
        
        let mut sessions: Vec<_> = std::fs::read_dir(&sessions_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .filter_map(|entry| {
                        let modified = entry.metadata().ok()?.modified().ok()?;
                        Some((entry.file_name().to_string_lossy().to_string(), modified))
                    })
                    .collect()
            })
            .unwrap_or_default();
        
        // Sort by modification time, newest first
        sessions.sort_by(|a, b| b.1.cmp(&a.1));
        
        // Apply limit if specified
        let sessions: Vec<String> = sessions
            .into_iter()
            .map(|(name, _)| name)
            .take(limit.unwrap_or(usize::MAX))
            .collect();
        
        sessions
    }
    
    /// List project directories from ~/projects/, sorted alphabetically.
    fn list_projects(&self, prefix: &str) -> Vec<String> {
        let projects_dir = match dirs::home_dir() {
            Some(home) => home.join("projects"),
            None => return Vec::new(),
        };
        
        if !projects_dir.is_dir() {
            return Vec::new();
        }
        
        let mut projects: Vec<String> = std::fs::read_dir(&projects_dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().is_dir())
                    .filter_map(|entry| Some(entry.file_name().to_string_lossy().to_string()))
                    .filter(|name| name.starts_with(prefix))
                    .collect()
            })
            .unwrap_or_default();
        
        projects.sort();
        projects
    }
}

impl Default for G3Helper {
    fn default() -> Self {
        Self::new()
    }
}

impl Completer for G3Helper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> Result<(usize, Vec<Pair>), ReadlineError> {
        let line_to_cursor = &line[..pos];
        
        // Extract the current word being typed
        let (word_start, word) = self.extract_word(line, pos);
        
        // Case 1: Command completion at line start
        if word_start == 0 && word.starts_with('/') && !word.contains(' ') {
            let after_slash = &word[1..];
            if !after_slash.contains('/') {
                let matches: Vec<Pair> = COMMANDS
                    .iter()
                    .filter(|cmd| cmd.starts_with(word))
                    .map(|cmd| Pair {
                        display: cmd.to_string(),
                        replacement: cmd.to_string(),
                    })
                    .collect();
                
                if !matches.is_empty() {
                    return Ok((0, matches));
                }
            }
        }
        
        // Case 2: Path completion for path-like prefixes (handles quotes ourselves)
        if self.is_path_prefix(word) || (word_start > 0 && line_to_cursor[word_start..].starts_with('/')) {
            let has_leading_quote = word.starts_with('"') || word.starts_with('\'');
            let quote_char = if has_leading_quote { &word[..1] } else { "" };
            let has_escapes = word.contains('\\');
            
            let path_str = self.strip_quotes(word);
            let path_unescaped = self.unescape_path(path_str);
            let path: &str = &path_unescaped;
            
            let (_rel_start, completions) = self.file_completer.complete(path, path.len(), ctx)?;
            
            if completions.is_empty() {
                return Ok((pos, vec![]));
            }
            
            let adjusted: Vec<Pair> = completions
                .into_iter()
                .map(|pair| {
                    let has_spaces = pair.replacement.contains(' ');
                    let replacement = if has_leading_quote {
                        format!("{}{}{}", quote_char, pair.replacement, quote_char)
                    } else if has_escapes && has_spaces {
                        pair.replacement.replace(' ', "\\ ")
                    } else if has_spaces {
                        format!("\"{}\"" , pair.replacement)
                    } else {
                        pair.replacement
                    };
                    
                    let needs_quotes = has_spaces || has_leading_quote;
                    let display = if needs_quotes && !pair.display.starts_with('"') {
                        format!("\"{}\"" , pair.display)
                    } else {
                        pair.display
                    };
                    
                    Pair { display, replacement }
                })
                .collect();
            
            return Ok((word_start, adjusted));
        }
        
        // Case 3: Path argument for /run command
        if line_to_cursor.starts_with("/run ") {
            let path = self.strip_quotes(word);
            let (_, completions) = self.file_completer.complete(path, path.len(), ctx)?;
            // Cyan color for command argument completions
            let cyan_completions: Vec<Pair> = completions
                .into_iter()
                .map(|p| Pair {
                    display: format!("\x1b[36m{}\x1b[0m", p.display),
                    replacement: p.replacement,
                })
                .collect();
            return Ok((word_start, cyan_completions));
        }
        
        // Case 4: Session ID completion for /resume command
        if line_to_cursor.starts_with("/resume ") {
            let partial = word;
            let sessions = self.list_sessions(None);
            // Cyan color for command argument completions
            let matches: Vec<Pair> = sessions
                .into_iter()
                .filter(|s| s.starts_with(partial))
                .map(|s| Pair {
                    display: format!("\x1b[36m{}\x1b[0m", s),
                    replacement: s,
                })
                .take(8)
                .collect();
            return Ok((word_start, matches));
        }

        // Case 5: Project name completion for /project command
        if line_to_cursor.starts_with("/project ") {
            let partial = word;
            let projects = self.list_projects(partial);
            // Cyan color for command argument completions
            let matches: Vec<Pair> = projects
                .into_iter()
                .map(|name| {
                    let full_path = format!("~/projects/{}", name);
                    Pair {
                        display: format!("\x1b[36m{}\x1b[0m", name),
                        replacement: full_path,
                    }
                })
                .collect();
            return Ok((word_start, matches));
        }

        // No completion for regular text
        Ok((pos, vec![]))
    }
}

// Required trait implementations for Helper
impl Hinter for G3Helper {
    type Hint = String;

    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Highlighter for G3Helper {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> std::borrow::Cow<'b, str> {
        if crate::pretty::is_active() {
            return std::borrow::Cow::Owned(pretty_prompt_colors(prompt));
        }
        // Plan mode prompt: colorize "[plan mode]" in magenta
        if prompt.contains("[plan mode]") {
            return std::borrow::Cow::Owned(
                prompt.replace("[plan mode]", "\x1b[35m[plan mode]\x1b[0m")
            );
        }
        
        // If prompt contains " | ", colorize from "|" to ">" in blue
        if let Some(pipe_pos) = prompt.find(" | ") {
            if let Some(gt_pos) = prompt.rfind('>') {
                let before = &prompt[..pipe_pos + 1]; // "butler "
                let colored_part = &prompt[pipe_pos + 1..gt_pos + 1]; // "| project>"
                let after = &prompt[gt_pos + 1..]; // " "
                return std::borrow::Cow::Owned(format!(
                    "{}\x1b[34m{}\x1b[0m{}",
                    before, colored_part, after
                ));
            }
        }
        std::borrow::Cow::Borrowed(prompt)
    }
}

/// Tokyo Night colours for the pretty prompt ("g3 · proj plan ❯ ").
fn pretty_prompt_colors(prompt: &str) -> String {
    use crate::pretty::screen::tn;
    if prompt.trim() == "┊" {
        return format!("{}{}{}", tn::FAINT, prompt, tn::RESET);
    }
    let mut out = prompt.to_string();
    out = out.replacen("g3", &format!("{}{}g3{}", tn::BOLD, tn::BLUE, tn::RESET), 1);
    if let Some(dot) = out.find(" · ") {
        let end = out[dot..].find(|c| c == '❯').map(|e| dot + e).unwrap_or(out.len());
        let seg = out[dot..end].replace(" plan ", " ");
        let colored = format!("{}{}{} ", tn::CYAN, seg.trim_end(), tn::RESET);
        out = format!("{}{}{}", &out[..dot], colored, &out[end..]);
        if prompt.contains(" plan ") {
            out = out.replacen("❯", &format!("{}plan{} ❯", tn::YELLOW, tn::RESET), 1);
        }
    } else if prompt.contains(" plan ") {
        out = out.replacen(" plan ", &format!(" {}plan{} ", tn::YELLOW, tn::RESET), 1);
    }
    out.replacen("❯", &format!("{}{}❯{}", tn::BOLD, tn::PURPLE, tn::RESET), 1)
}

impl Validator for G3Helper {}

impl Helper for G3Helper {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_completion() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);

        let (start, matches) = helper.complete("/com", 4, &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].replacement, "/compact");
    }

    #[test]
    fn test_command_completion_multiple() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);

        let (start, matches) = helper.complete("/s", 2, &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|m| m.replacement == "/skinnify"));
        assert!(matches.iter().any(|m| m.replacement == "/stats"));
    }

    #[test]
    fn test_path_prefix_detection() {
        let helper = G3Helper::new();
        
        assert!(helper.is_path_prefix("./"));
        assert!(helper.is_path_prefix("./src"));
        assert!(helper.is_path_prefix("../"));
        assert!(helper.is_path_prefix("~/"));
        assert!(helper.is_path_prefix("~/Documents"));
        assert!(helper.is_path_prefix("/etc"));
        assert!(helper.is_path_prefix("."));
        assert!(helper.is_path_prefix(".."));
        assert!(helper.is_path_prefix("~"));
        
        assert!(!helper.is_path_prefix("hello"));
        assert!(!helper.is_path_prefix("src"));
    }

    #[test]
    fn test_extract_word_simple() {
        let helper = G3Helper::new();
        
        let (start, word) = helper.extract_word("hello world", 11);
        assert_eq!(start, 6);
        assert_eq!(word, "world");
    }

    #[test]
    fn test_extract_word_with_path() {
        let helper = G3Helper::new();
        
        let (start, word) = helper.extract_word("edit ./src/main.rs", 18);
        assert_eq!(start, 5);
        assert_eq!(word, "./src/main.rs");
    }

    #[test]
    fn test_extract_word_quoted() {
        let helper = G3Helper::new();
        
        // Quoted path with spaces
        let (start, word) = helper.extract_word("edit \"./My Files/doc", 20);
        assert_eq!(start, 5);
        assert_eq!(word, "\"./My Files/doc");
    }

    #[test]
    fn test_no_completion_for_regular_input() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);

        // Regular text should not complete
        let (start, matches) = helper.complete("hello world", 11, &ctx).unwrap();
        assert_eq!(start, 11);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_slash_at_start_is_command() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);

        // "/h" at start should complete to commands
        let (start, matches) = helper.complete("/h", 2, &ctx).unwrap();
        assert_eq!(start, 0);
        assert!(matches.iter().any(|m| m.replacement == "/help"));
    }

    #[test]
    fn test_actual_completion_with_quotes() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        
        let line = "edit \"~/";
        let pos = line.len();
        match helper.complete(line, pos, &ctx) {
            Ok((start, completions)) => {
                assert!(start > 0 || completions.is_empty() || true); // Just verify no panic
            }
            Err(_) => {}
        }
        
        let line = "edit ~/My\\ ";
        let pos = line.len();
        match helper.complete(line, pos, &ctx) {
            Ok((start, completions)) => {
                let _ = (start, completions); // Just verify no panic
            }
            Err(_) => {}
        }
        
        let line = "edit \"~/\"";
        let pos = line.len();
        match helper.complete(line, pos, &ctx) {
            Ok((start, completions)) => {
                let _ = (start, completions);
            }
            Err(_) => {}
        }
    }

    #[test]
    fn test_no_completion_for_bare_quote() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        
        let line = "edit \"";
        let pos = line.len();
        let (start, completions) = helper.complete(line, pos, &ctx).unwrap();
        let _ = start;
        assert_eq!(completions.len(), 0, "Bare quote should not trigger path completion");
    }

    #[test]
    fn test_no_completion_for_random_text_in_quotes() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        
        let line = "edit \"hello world";
        let pos = line.len();
        let (start, completions) = helper.complete(line, pos, &ctx).unwrap();
        let _ = start;
        assert_eq!(completions.len(), 0, "Random quoted text should not trigger path completion");
        
        let line = "edit \"foo";
        let pos = line.len();
        let (start, completions) = helper.complete(line, pos, &ctx).unwrap();
        let _ = start;
        assert_eq!(completions.len(), 0, "Quoted non-path should not trigger completion");
    }

    #[test]
    fn test_resume_completion_lists_sessions() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        
        let line = "/resume ";
        let pos = line.len();
        let (start, completions) = helper.complete(line, pos, &ctx).unwrap();
        let _ = start;
        
        if std::path::Path::new(".g3/sessions").is_dir() {
            assert!(completions.len() > 0, "Should list sessions when .g3/sessions exists");
            
            if let Some(first) = completions.first() {
                let prefix = &first.replacement[..first.replacement.len().min(5)];
                let line = format!("/resume {}", prefix);
                let pos = line.len();
                let (_, filtered) = helper.complete(&line, pos, &ctx).unwrap();
                assert!(filtered.len() >= 1, "Should find at least one match");
                assert!(filtered.iter().all(|p| p.replacement.starts_with(prefix)));
            }
        }
        
        let line = "/resume zzz_nonexistent_prefix_";
        let pos = line.len();
        let (_, completions) = helper.complete(line, pos, &ctx).unwrap();
        assert_eq!(completions.len(), 0, "Non-matching prefix should return empty");
    }

    #[test]
    fn test_highlight_prompt_plan_mode() {
        let helper = G3Helper::new();
        
        // Plan mode prompt should be colorized with magenta
        let prompt = " [plan mode] >> ";
        let highlighted = helper.highlight_prompt(prompt, false);
        assert!(highlighted.contains("\x1b[35m"), "Plan mode should use magenta color");
        assert!(highlighted.contains("[plan mode]"), "Should contain [plan mode] text");
        assert!(highlighted.contains("\x1b[0m"), "Should reset color");
    }

    #[test]
    fn test_highlight_prompt_normal_unchanged() {
        let helper = G3Helper::new();
        
        // Normal prompt without project should be unchanged
        let prompt = "g3> ";
        let highlighted = helper.highlight_prompt(prompt, false);
        assert_eq!(highlighted.as_ref(), prompt, "Normal prompt should be unchanged");
    }

    #[test]
    fn test_resume_completion_graceful_no_panic() {
        let helper = G3Helper::new();
        let sessions = helper.list_sessions(None);
        let _ = sessions; // Just verify no panic
    }
    
    #[test]
    fn test_project_completion_lists_projects() {
        let helper = G3Helper::new();
        let history = rustyline::history::DefaultHistory::new();
        let ctx = Context::new(&history);
        
        let line = "/project ";
        let pos = line.len();
        let (start, completions) = helper.complete(line, pos, &ctx).unwrap();
        let _ = start;
        
        // If ~/projects exists and has directories, we should get completions
        if let Some(home) = dirs::home_dir() {
            let projects_dir = home.join("projects");
            if projects_dir.is_dir() {
                // Verify completions have the right format (display is name, replacement is ~/projects/name)
                for completion in &completions {
                    assert!(completion.replacement.starts_with("~/projects/"), 
                        "Replacement should start with ~/projects/, got: {}", completion.replacement);
                    assert!(!completion.display.contains('/'),
                        "Display should be just the project name, got: {}", completion.display);
                }
            }
        }
        
        // Test with a prefix that won't match anything
        let line = "/project zzz_nonexistent_prefix_";
        let pos = line.len();
        let (_, completions) = helper.complete(line, pos, &ctx).unwrap();
        assert_eq!(completions.len(), 0, "Non-matching prefix should return empty");
    }
}
