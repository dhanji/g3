//! Human labels for tool calls: "📄 reading lib.rs" while it runs,
//! "📄 read lib.rs" once it's done. Pure — no terminal, no state.
//!
//! Never panics on any input shape: an unknown tool or garbage args get a
//! generic label, because a display helper must not be able to kill a turn.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    pub icon: &'static str,
    pub doing: String,
    pub done: String,
    pub detail: String,
}

impl Explained {
    fn new(icon: &'static str, doing: &str, done: &str, detail: impl Into<String>) -> Self {
        Self { icon, doing: doing.to_string(), done: done.to_string(), detail: short(&detail.into(), 48) }
    }
}

/// Collapse whitespace and clip to `n` display chars with an ellipsis.
pub fn short(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn basename(p: &str) -> String {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// butler's own tool scripts, matched anywhere in a shell command.
/// (name, icon, doing, done, [(subcommand, doing, done)])
type Sub = (&'static str, &'static str, &'static str);
const BUTLER_TOOLS: &[(&str, &str, &str, &str, &[Sub])] = &[
    ("email_reader", "📬", "checking mail", "checked mail", &[
        ("search", "searching mail", "searched mail"), ("read", "reading mail", "read mail"),
        ("thread", "reading a thread", "read a thread"), ("recent", "scanning the inbox", "scanned the inbox")]),
    ("email_sender", "📮", "sending mail", "sent mail", &[]),
    ("email_voice_stats", "🎤", "linting the draft", "linted the draft", &[]),
    ("task", "📋", "updating a task", "updated a task", &[
        ("list", "checking tasks", "checked tasks"), ("new", "creating a task", "created a task"),
        ("context", "reading a task", "read a task"), ("drop", "dropping a task", "dropped a task")]),
    ("topic", "🧭", "consulting topics", "consulted topics", &[
        ("note", "noting on a topic", "noted on a topic"), ("brief", "updating a topic", "updated a topic")]),
    ("lookup", "🔎", "looking up", "looked up", &[]),
    ("calendar_client", "📅", "checking the calendar", "checked the calendar", &[
        ("add", "adding to the calendar", "added to the calendar")]),
    ("ask_web", "🌐", "asking the web", "asked the web", &[]),
    ("draft", "📝", "drafting", "drafted", &[]),
    ("imessage_client", "💬", "reading iMessages", "read iMessages", &[]),
    ("whatsapp_client", "💬", "reading WhatsApp", "read WhatsApp", &[]),
    ("contacts_client", "👤", "checking contacts", "checked contacts", &[]),
    ("library", "📚", "checking the library", "checked the library", &[]),
    ("storypark", "🧸", "reading Storypark", "read Storypark", &[]),
    ("notes_client", "📒", "reading notes", "read notes", &[]),
    ("gdrive_client", "📁", "checking Drive", "checked Drive", &[]),
    ("preview", "👀", "showing a picture", "showed a picture", &[]),
    ("android", "📱", "driving the phone", "drove the phone", &[]),
    ("weather", "⛅", "checking the weather", "checked the weather", &[]),
];

/// Programs → (icon, doing, done). Checked against the first real word.
const PROGRAMS: &[(&[&str], &str, &str, &str)] = &[
    (&["rg", "grep", "fd", "find", "ag"], "🔎", "searching files", "searched files"),
    (&["cat", "head", "tail", "sed", "less", "bat", "wc", "awk", "jq", "cut", "sort", "diff"], "📄", "reading", "read"),
    (&["ls", "eza", "tree", "du", "stat", "file"], "📂", "looking around", "looked around"),
    (&["curl", "wget", "http"], "🌐", "fetching", "fetched"),
    (&["osascript"], "🍎", "scripting macOS", "scripted macOS"),
    (&["op"], "🔐", "consulting 1Password", "consulted 1Password"),
    (&["brew", "apt", "apt-get", "pip", "pip3", "uv", "pkg"], "📦", "installing", "installed"),
    (&["npm", "pnpm", "yarn", "npx", "bun"], "📦", "running npm", "ran npm"),
    (&["make", "cmake", "ninja", "just"], "🔨", "building", "built"),
    (&["docker", "podman"], "🐳", "wrangling containers", "wrangled containers"),
    (&["gh"], "🐙", "talking to GitHub", "talked to GitHub"),
    (&["go"], "🐹", "running go", "ran go"),
    (&["open"], "🔗", "opening", "opened"),
    (&["pdftotext", "tesseract", "textutil", "pandoc"], "📑", "extracting text", "extracted text"),
    (&["mkdir", "cp", "mv", "touch", "ln", "chmod", "tar", "unzip", "zip"], "📁", "shuffling files", "shuffled files"),
    (&["rm"], "🧹", "tidying up", "tidied up"),
    (&["kill", "pkill", "ps", "pgrep", "lsof"], "🔧", "checking processes", "checked processes"),
    (&["launchctl", "systemctl", "tailscale"], "🔧", "poking a service", "poked a service"),
    (&["date", "cal", "echo", "printf", "true", "sleep", "which", "type", "env"], "⌛", "checking", "checked"),
];

const WRAPPERS: &[&str] = &["sudo", "timeout", "env", "nice", "time", "command", "exec", "nohup", "caffeinate"];
const SKIP_CMDS: &[&str] = &["cd", "set", "export", "source", ".", "pushd", "popd"];

fn split_words(seg: &str) -> Vec<String> {
    // A tiny shell-ish splitter: honours single/double quotes, nothing else.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for ch in seg.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => { quote = Some(ch); any = true; }
            None if ch.is_whitespace() => {
                if !cur.is_empty() || any { out.push(std::mem::take(&mut cur)); any = false; }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() || any { out.push(cur); }
    out
}

fn is_assignment(w: &str) -> bool {
    let mut chars = w.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    w.contains('=') && w.split('=').next().map(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).unwrap_or(false)
}

fn is_duration(w: &str) -> bool {
    let t = w.trim_start_matches('-').trim_end_matches(|c| c == 's' || c == 'm' || c == 'h');
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The first interesting simple command: skips `cd x &&`, env assignments,
/// wrappers (timeout/sudo) and their numeric args.
fn main_words(cmd: &str) -> Vec<String> {
    let first_line = cmd.lines().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).unwrap_or("");
    for seg in first_line.split(|c| c == ';').flat_map(|s| s.split("&&")).flat_map(|s| s.split("||")) {
        let pipe_head = seg.split('|').next().unwrap_or("");
        let mut words = split_words(pipe_head.trim());
        while let Some(w) = words.first() {
            let b = basename(w);
            if is_assignment(w) || WRAPPERS.contains(&b.as_str()) || is_duration(w) {
                words.remove(0);
            } else {
                break;
            }
        }
        match words.first() {
            None => continue,
            Some(w) if SKIP_CMDS.contains(&basename(w).as_str()) => continue,
            Some(_) => return words,
        }
    }
    Vec::new()
}

fn first_quoted(s: &str) -> String {
    for q in ['"', '\''] {
        if let Some(a) = s.find(q) {
            if let Some(len) = s[a + 1..].find(q) {
                let inner = &s[a + 1..a + 1 + len];
                if !inner.is_empty() && !inner.contains('\n') && inner.chars().count() <= 80 {
                    return inner.to_string();
                }
            }
        }
    }
    String::new()
}

fn first_positional(words: &[String]) -> String {
    words.iter().skip(1).find(|w| !w.starts_with('-')).cloned().unwrap_or_default()
}

fn butler_tool(cmd: &str) -> Option<Explained> {
    for (name, icon, doing, done, subs) in BUTLER_TOOLS {
        let needle = format!("{}.py", name);
        let Some(pos) = cmd.find(&needle) else { continue };
        // must be a word boundary before it: start, '/', or whitespace
        if pos > 0 {
            let prev = cmd[..pos].chars().last().unwrap_or(' ');
            if !(prev == '/' || prev.is_whitespace()) {
                continue;
            }
        }
        let rest = &cmd[pos + needle.len()..];
        let rest_words = split_words(rest.lines().next().unwrap_or(""));
        let sub = rest_words.first().cloned().unwrap_or_default();
        let (mut d, mut dn) = (doing.to_string(), done.to_string());
        if let Some((_, sd, sdn)) = subs.iter().find(|(s, _, _)| *s == sub) {
            d = sd.to_string();
            dn = sdn.to_string();
        }
        let mut detail = first_quoted(rest);
        if detail.is_empty() && subs.is_empty() && !sub.starts_with('-') && !sub.contains('>') {
            detail = sub; // no subcommands: the first bare word IS the argument
        }
        return Some(Explained::new(icon, &d, &dn, detail));
    }
    None
}

fn cargo(words: &[String]) -> Explained {
    let sub = first_positional(words);
    let pkg = words.iter().position(|w| w == "-p" || w == "--package")
        .and_then(|i| words.get(i + 1)).cloned()
        .or_else(|| words.iter().find_map(|w| w.strip_prefix("--bin=").map(String::from)))
        .unwrap_or_default();
    let (doing, done) = match sub.as_str() {
        "test" | "t" | "nextest" => ("testing", "tested"),
        "build" | "b" => ("building", "built"),
        "run" | "r" => ("running", "ran"),
        "check" | "c" => ("checking", "checked"),
        "clippy" => ("linting", "linted"),
        "fmt" => ("formatting", "formatted"),
        "install" => ("installing", "installed"),
        "add" => ("adding a crate", "added a crate"),
        _ => ("running cargo", "ran cargo"),
    };
    Explained::new("🦀", doing, done, pkg)
}

/// Explain a shell command.
pub fn explain_shell(cmd: &str) -> Explained {
    if let Some(e) = butler_tool(cmd) {
        return e;
    }
    let words = main_words(cmd);
    let Some(first) = words.first() else {
        return Explained::new("🔩", "running a command", "ran a command", "");
    };
    let prog = basename(first);
    if prog.starts_with("python") || matches!(prog.as_str(), "node" | "bash" | "sh" | "zsh" | "ruby" | "deno") {
        let script = first_positional(&words);
        if script.is_empty() || words.iter().any(|w| w == "-c" || w == "-e") && !script.contains('.') {
            return Explained::new("🐍", "running a snippet", "ran a snippet", "");
        }
        let name = basename(&script);
        return Explained::new("🐍", &format!("running {}", name), &format!("ran {}", name), "");
    }
    match prog.as_str() {
        "cargo" => return cargo(&words),
        "ssh" | "mosh" => {
            let mut host = String::new();
            let mut i = 1;
            while i < words.len() {
                let w = &words[i];
                if w == "-o" || w == "-p" || w == "-i" || w == "-l" || w == "-J" || w == "-F" {
                    i += 2;
                    continue;
                }
                if !w.starts_with('-') {
                    host = w.rsplit('@').next().unwrap_or(w).to_string();
                    break;
                }
                i += 1;
            }
            if host.is_empty() {
                host = "a box".into();
            }
            return Explained::new("📡", &format!("working on {}", host), &format!("worked on {}", host), "");
        }
        "scp" | "rsync" => return Explained::new("📡", "copying", "copied", ""),
        "git" => {
            let mut sub = String::new();
            let mut i = 1;
            while i < words.len() {
                let w = &words[i];
                if w == "-C" || w == "-c" || w == "--git-dir" || w == "--work-tree" {
                    i += 2;
                    continue;
                }
                if !w.starts_with('-') {
                    sub = w.clone();
                    break;
                }
                i += 1;
            }
            let label = if sub.is_empty() { "git".to_string() } else { format!("git {}", sub) };
            return Explained::new("🌿", &label, &label, "");
        }
        _ => {}
    }
    for (names, icon, doing, done) in PROGRAMS {
        if names.contains(&prog.as_str()) {
            let detail = match *icon {
                "🔎" => {
                    let q = first_quoted(cmd);
                    if q.is_empty() { first_positional(&words) } else { q }
                }
                "📄" | "📂" => words.iter().skip(1).filter(|w| !w.starts_with('-') && (w.contains('/') || w.contains('.')))
                    .last().map(|w| basename(w)).unwrap_or_default(),
                "🌐" => words.iter().find(|w| w.contains("://"))
                    .map(|u| u.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            return Explained::new(icon, doing, done, detail);
        }
    }
    Explained::new("🔩", &format!("running {}", prog), &format!("ran {}", prog), "")
}

fn arg_str(args: &Value, key: &str) -> String {
    match args.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

/// Explain any tool call. `args` may be anything (or Null).
pub fn explain_tool(name: &str, args: &Value) -> Explained {
    let name = if name.is_empty() { "tool" } else { name };
    match name {
        "shell" => explain_shell(&arg_str(args, "command")),
        "read_file" => Explained::new("📄", "reading", "read", basename(&arg_str(args, "file_path"))),
        "write_file" => Explained::new("📝", "writing", "wrote", basename(&arg_str(args, "file_path"))),
        "str_replace" => Explained::new("🩹", "editing", "edited", basename(&arg_str(args, "file_path"))),
        "read_image" => Explained::new("👀", "looking at", "looked at", basename(&arg_str(args, "file_paths"))),
        "code_search" => {
            let detail = args.get("searches").and_then(|s| s.as_array()).and_then(|a| a.first())
                .and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("").to_string();
            Explained::new("🔎", "searching code", "searched code", detail)
        }
        "remember" => Explained::new("🧠", "remembering", "remembered", ""),
        "screenshot" => Explained::new("📸", "taking a screenshot", "took a screenshot", ""),
        "coverage" => Explained::new("📊", "measuring coverage", "measured coverage", ""),
        "plan_read" => Explained::new("📐", "reading the plan", "read the plan", ""),
        "plan_write" => Explained::new("📐", "updating the plan", "updated the plan", ""),
        "plan_approve" => Explained::new("📐", "approving the plan", "approved the plan", ""),
        "todo_read" => Explained::new("📌", "reading todos", "read todos", ""),
        "todo_write" => Explained::new("📌", "updating todos", "updated todos", ""),
        "write_envelope" => Explained::new("📨", "sealing the envelope", "sealed the envelope", ""),
        "load_toolset" => Explained::new("🧰", "loading tools", "loaded tools", arg_str(args, "name")),
        "background_process" => Explained::new("🚀", "starting", "started", arg_str(args, "name")),
        "research" => Explained::new("🔬", "researching", "researched", arg_str(args, "query")),
        n if n.starts_with("webdriver") => Explained::new("🧭", "browsing", "browsed", ""),
        other => {
            let pretty = other.replace('_', " ");
            Explained::new("🔧", &pretty, &pretty, "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sh(c: &str) -> Explained {
        explain_tool("shell", &json!({ "command": c }))
    }

    #[test]
    fn cargo_test_with_package() {
        let e = sh("cd ~/src/g3 && cargo test -p g3-cli --lib 2>&1 | tail -20");
        assert_eq!((e.icon, e.doing.as_str(), e.done.as_str(), e.detail.as_str()), ("🦀", "testing", "tested", "g3-cli"));
    }

    #[test]
    fn git_skips_dash_c() {
        assert_eq!(sh("git -C crates/g3-cli log --oneline -5").doing, "git log");
        assert_eq!(sh("git status --short").done, "git status");
    }

    #[test]
    fn wrappers_and_env_skipped() {
        let e = sh("FOO=1 /opt/homebrew/bin/timeout 10 rg -n 'needle' src");
        assert_eq!((e.icon, e.doing.as_str(), e.detail.as_str()), ("🔎", "searching files", "needle"));
    }

    #[test]
    fn ssh_host_after_options() {
        assert_eq!(sh("ssh -o BatchMode=yes -p 2222 dhanji@100.92.52.93 'uptime'").doing, "working on 100.92.52.93");
    }

    #[test]
    fn butler_scripts() {
        let e = sh("~/.butler/bin/python3 tools/email_reader.py search \"tax\" --limit 5");
        assert_eq!((e.doing.as_str(), e.detail.as_str()), ("searching mail", "tax"));
        let e = sh("~/.butler/bin/python3 tools/lookup.py charm 2>&1 | head");
        assert_eq!((e.doing.as_str(), e.detail.as_str()), ("looking up", "charm"));
        assert_eq!(sh("$P tools/task.py new 'x' --tag y").doing, "creating a task");
        // not a word boundary: must not match "mytask.py"
        assert_ne!(sh("python3 mytask.py").doing, "updating a task");
    }

    #[test]
    fn reading_detail_is_basename() {
        assert_eq!(sh("sed -n 1,20p ~/src/charm/build.sh").detail, "build.sh");
        let e = explain_tool("read_file", &json!({"file_path": "/a/b/lib.rs", "start": 0}));
        assert_eq!((e.doing.as_str(), e.detail.as_str()), ("reading", "lib.rs"));
    }

    #[test]
    fn python_script_and_snippet() {
        assert_eq!(sh("~/.butler/bin/python3 tools/weird_thing.py --x").doing, "running weird_thing.py");
        assert_eq!(sh("python3 -c 'print(1)'").doing, "running a snippet");
    }

    #[test]
    fn garbage_never_panics() {
        for (n, a) in [("shell", json!(null)), ("shell", json!({"command": "'unterminated"})), ("", json!("str")),
                       ("mystery_tool", json!({"x": 1})), ("read_image", json!({"file_paths": []})),
                       ("shell", json!({"command": ""})), ("shell", json!({"command": "&& ; | "})),
                       ("code_search", json!({"searches": "nope"}))] {
            let e = explain_tool(n, &a);
            assert!(!e.doing.is_empty());
        }
        assert_eq!(explain_tool("mystery_tool", &json!({})).doing, "mystery tool");
    }

    /// Icons must be ONE codepoint that terminals agree is 2 columns wide.
    /// Variation-selector emoji (✏️ = U+270F U+FE0F) and text-presentation
    /// symbols (🛰) are measured differently by terminals and by us, which
    /// garbled the line in a live run.
    #[test]
    fn icons_are_single_wide_codepoints() {
        use unicode_width::UnicodeWidthStr;
        let src = include_str!("explain.rs");
        let mut checked = 0;
        for chunk in src.split("Explained::new(\"").skip(1) {
            let icon: String = chunk.chars().take_while(|c| *c != '"').collect();
            if icon.is_empty() || icon.starts_with('{') { continue; }
            checked += 1;
            assert_eq!(icon.chars().count(), 1, "icon {:?} is not a single codepoint", icon);
            assert_eq!(UnicodeWidthStr::width(icon.as_str()), 2, "icon {:?} is not 2 columns wide", icon);
        }
        for (_, icon, _, _, _) in BUTLER_TOOLS { assert_eq!(UnicodeWidthStr::width(*icon), 2, "{:?}", icon); }
        for (_, icon, _, _) in PROGRAMS { assert_eq!(UnicodeWidthStr::width(*icon), 2, "{:?}", icon); }
        assert!(checked > 20);
    }

    #[test]
    fn long_detail_is_clipped_and_multiline_uses_first_line() {
        let e = sh(&format!("rg '{}' .", "x".repeat(70)));
        assert!(e.detail.chars().count() <= 48 && e.detail.ends_with('…'));
        assert_eq!(sh("# comment\ncargo build --release\necho done").doing, "building");
    }
}
