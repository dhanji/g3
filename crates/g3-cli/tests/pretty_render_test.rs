//! Screen-level tests for the pretty UI: drive the real PrettyUiWriter, feed
//! its bytes to a vt100 terminal emulator, and assert on what a person sees.

use g3_cli::pretty::{PrettyUiWriter, SharedBuf};
use g3_core::ui_writer::UiWriter;
use serde_json::json;

const W: u16 = 60;

struct Rig {
    w: PrettyUiWriter,
    buf: SharedBuf,
    width: u16,
}

impl Rig {
    fn new(width: u16) -> Self {
        let buf = SharedBuf::default();
        Self { w: PrettyUiWriter::with_sink(buf.clone(), width as usize), buf, width }
    }
    /// Stream text the way the provider does: small chunks, frames in between.
    fn stream(&self, text: &str, chunk: usize) {
        let chars: Vec<char> = text.chars().collect();
        for c in chars.chunks(chunk) {
            self.w.print_agent_response(&c.iter().collect::<String>());
            self.w.tick();
        }
    }
    fn settle(&self) {
        for _ in 0..2000 {
            if self.w.pending_len() == 0 {
                break;
            }
            self.w.tick();
        }
        self.w.tick();
    }
    fn screen(&self) -> Vec<String> {
        let mut p = vt100::Parser::new(400, self.width, 0);
        p.process(self.buf.text().as_bytes());
        let s = p.screen();
        let mut lines: Vec<String> = s.rows(0, self.width).collect();
        while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            lines.pop();
        }
        lines.iter().map(|l| l.trim_end().to_string()).collect()
    }
    fn text(&self) -> String {
        self.screen().join("\n")
    }
}

#[test]
fn prose_renders_markdown_with_gutter_and_wraps_at_words() {
    let r = Rig::new(40);
    r.w.begin_turn();
    let para = "The quick brown fox jumps over the lazy dog while **butler** watches the deck and sips coffee slowly.\n";
    r.stream(para, 5);
    r.w.finish_streaming_markdown();
    let lines = r.screen();
    let body: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    assert!(body.len() >= 3, "expected wrapping at 40 cols: {:?}", lines);
    for l in &body {
        assert!(l.starts_with("  ") && !l.starts_with("   "), "gutter must be exactly 2: {:?}", l);
        assert!(l.chars().count() <= 39, "line too wide: {:?}", l);
    }
    let joined: String = body.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    assert_eq!(joined, "The quick brown fox jumps over the lazy dog while butler watches the deck and sips coffee slowly.");
    assert!(!r.text().contains("**"));
    assert!(!r.text().contains('▌'), "cursor must be erased after finish");
}

#[test]
fn tool_start_flushes_text_first_never_reorders() {
    let r = Rig::new(W);
    r.w.begin_turn();
    // feed a lot of text, but do NOT let the typewriter catch up
    r.w.print_agent_response("Let me look at the parser before changing anything here.\n");
    r.w.print_tool_header("read_file", Some(&json!({"file_path": "/x/src/parser.rs", "start": 0, "end": 200})));
    r.w.print_tool_arg("file_path", "/x/src/parser.rs");
    r.w.print_tool_arg("start", "0");
    r.w.print_tool_arg("end", "200");
    r.w.print_tool_compact("read_file", "200 lines", "3ms", 120, 4.0);
    let t = r.text();
    let a = t.find("Let me look at the parser").expect("text lost");
    let b = t.find("✓ 📄 read  parser.rs [0..200]").expect(&format!("tool line missing:\n{}", t));
    assert!(a < b, "text must precede the tool line:\n{}", t);
    assert!(t.contains("200 lines") && t.contains("3ms"));
}

#[test]
fn shell_tool_explained_and_collapsed() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.w.print_tool_header("shell", Some(&json!({"command": "cargo test -p g3-cli --lib"})));
    r.w.print_tool_output_header();
    r.w.tick();
    assert!(r.text().contains("🦀 testing  g3-cli"), "running status:\n{}", r.text());
    r.w.update_tool_output_line("test result: ok. 42 passed");
    r.w.tick();
    assert!(r.text().contains("⟩ test result: ok. 42 passed"));
    r.w.print_tool_timing("4.2s", 300, 5.0);
    r.w.tick();
    let t = r.text();
    assert!(t.contains("✓ 🦀 tested  g3-cli  4.2s"), "{}", t);
    assert!(!t.contains("🦀 testing"), "running status must be erased:\n{}", t);
}

#[test]
fn failed_tool_gets_a_red_cross_and_its_last_line() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.w.print_tool_header("shell", Some(&json!({"command": "git push"})));
    r.w.update_tool_output_line("❌ Command failed: rejected");
    r.w.print_tool_timing("1.1s", 10, 5.0);
    let t = r.text();
    assert!(t.contains("✗ 🌿 git push"), "{}", t);
    assert!(t.contains("⟩ ❌ Command failed: rejected"), "{}", t);
}

#[test]
fn str_replace_summary_is_coloured_plus_minus() {
    let r = Rig::new(W);
    r.w.print_tool_header("str_replace", Some(&json!({"file_path": "/a/lib.rs"})));
    r.w.print_tool_arg("file_path", "/a/lib.rs");
    r.w.print_tool_compact("str_replace", "+5 | -3", "2ms", 50, 4.0);
    assert!(r.text().contains("✓ 🩹 edited  lib.rs  +5 −3  2ms"), "{}", r.text());
}

#[test]
fn read_further_continuation() {
    let r = Rig::new(W);
    for (s, e) in [("0", "100"), ("100", "200")] {
        r.w.print_tool_header("read_file", Some(&json!({"file_path": "/a/big.rs"})));
        r.w.print_tool_arg("file_path", "/a/big.rs");
        r.w.print_tool_arg("start", s);
        r.w.print_tool_arg("end", e);
        r.w.print_tool_compact("read_file", "100 lines", "1ms", 10, 4.0);
    }
    let t = r.text();
    assert!(t.contains("read  big.rs [0..100]"), "{}", t);
    assert!(t.contains("└ read further [100..200]"), "{}", t);
}

#[test]
fn thinking_spinner_then_footer_and_no_footer_without_a_turn() {
    let r = Rig::new(W);
    r.w.print_context_summary(1000, 1_000_000, 0.1); // before any turn: nothing
    assert_eq!(r.text().trim(), "", "no footer before a turn");
    r.w.begin_turn();
    r.w.tick();
    assert!(r.text().contains("thinking"), "{}", r.text());
    r.stream("Done!\n", 3);
    r.w.finish_streaming_markdown();
    r.w.print_context_summary(38_500, 1_000_000, 3.9);
    let t = r.text();
    assert!(!t.contains("thinking"), "spinner must be gone:\n{}", t);
    assert!(t.contains("─ ") && t.contains("ctx 4% 38.5k/1.0m"), "{}", t);
}

#[test]
fn cursor_never_drawn_in_last_column() {
    // width 20: gutter 2 + 17 chars puts the cursor at col 19 == width-1
    let r = Rig::new(20);
    r.w.begin_turn();
    r.w.print_agent_response(&"x".repeat(17));
    r.settle();
    let raw = r.buf.text();
    let after = &raw[raw.rfind('x').unwrap()..];
    assert!(!after.contains('▌'), "cursor drawn in the last column (would wrap): {:?}", after);
    // and with room to spare it IS drawn
    let r2 = Rig::new(20);
    r2.w.begin_turn();
    r2.w.print_agent_response("short");
    r2.settle();
    assert!(r2.buf.text().contains('▌'), "cursor should show mid-line");
}

#[test]
fn typewriter_catches_up_on_a_big_burst() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.w.print_agent_response(&"word ".repeat(1000));
    let mut frames = 0;
    while r.w.pending_len() > 0 {
        r.w.tick();
        frames += 1;
        assert!(frames < 120, "typewriter lags too far behind the model");
    }
}

#[test]
fn code_block_and_heading_survive_streaming() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.stream("## Plan\n\nHere:\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nAfter.\n", 4);
    r.w.finish_streaming_markdown();
    let t = r.text();
    assert!(t.contains("▍ Plan"), "heading mark:\n{}", t);
    assert!(t.contains("fn main() {"), "{}", t);
    assert!(t.contains("    println!(\"hi\");"), "code indentation kept:\n{}", t);
    assert!(t.contains("After."), "{}", t);
    assert!(!t.contains("```"), "{}", t);
}

#[test]
fn one_blank_line_between_text_and_tools() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.stream("Checking.\n", 4);
    r.w.print_tool_header("shell", Some(&json!({"command": "ls src"})));
    r.w.print_tool_timing("5ms", 1, 1.0);
    r.w.print_tool_header("shell", Some(&json!({"command": "ls tests"})));
    r.w.print_tool_timing("5ms", 1, 1.0);
    r.stream("Done.\n", 4);
    r.w.finish_streaming_markdown();
    let lines = r.screen();
    let i = lines.iter().position(|l| l.contains("Checking.")).unwrap();
    assert_eq!(lines[i + 1], "", "{:?}", lines);
    assert!(lines[i + 2].contains("✓ 📂"), "{:?}", lines);
    assert!(lines[i + 3].contains("✓ 📂"), "tools stack without gaps: {:?}", lines);
    assert_eq!(lines[i + 4], "", "{:?}", lines);
    assert!(lines[i + 5].contains("Done."), "{:?}", lines);
}

#[test]
fn plan_tree_renders() {
    let r = Rig::new(80);
    let yaml = "plan_id: demo\nrevision: 2\napproved_revision: 2\nitems:\n  - id: I1\n    description: first thing\n    state: done\n    touches: [a]\n    evidence: [src/a.rs]\n  - id: I2\n    description: second thing\n    state: doing\n    touches: [b]\n    checks:\n      happy: {desc: works, target: t}\n      negative: [{desc: fails well, target: t}]\n      boundary: [{desc: edges, target: t}]\n";
    r.w.print_plan_compact(Some(yaml), Some("/tmp/plan.yaml"), true);
    let t = r.text();
    for want in ["plan updated", "demo", "✓ approved@2", "1/2", "I1 first thing", "📎 src/a.rs", "◐ I2 second thing", "→ b", "✓ works", "✗ fails well", "◇ edges", "📄 /tmp/plan.yaml"] {
        assert!(t.contains(want), "missing {:?} in:\n{}", want, t);
    }
}

/// The remap gives headings a gutter mark by matching the formatter's heading
/// colours. If the formatter ever reuses one of those colours for something
/// else, that thing would sprout a mark — so pin that each is used once.
#[test]
fn heading_colours_are_used_only_for_headings() {
    let src = include_str!("../src/streaming_markdown.rs");
    for name in ["MAUVE", "BLUE", "LAVENDER"] {
        let uses = src.matches(&format!("{},", name)).count() + src.matches(&format!("{} ", name)).count();
        let total = src.match_indices(name).filter(|(i, _)| {
            let b = src.as_bytes();
            let before = if *i == 0 { b' ' } else { b[i - 1] };
            let after = b.get(i + name.len()).copied().unwrap_or(b' ');
            !(before.is_ascii_alphanumeric() || before == b'_') && !(after.is_ascii_alphanumeric() || after == b'_')
        }).count();
        let _ = uses;
        assert_eq!(total, 2, "{} should appear exactly twice (const + its heading) in streaming_markdown.rs", name);
    }
}

#[test]
fn wrapped_list_items_hang_under_their_text() {
    let r = Rig::new(40);
    r.w.begin_turn();
    r.stream("1. First item that is long enough to wrap onto another line here.\n- bullet that also wraps around the narrow screen nicely.\n", 6);
    r.w.finish_streaming_markdown();
    let lines = r.screen();
    let i = lines.iter().position(|l| l.starts_with("  1. ")).expect(&format!("{:?}", lines));
    assert!(lines[i + 1].starts_with("     ") && !lines[i + 1].starts_with("      "), "numbered continuation hangs at 5: {:?}", lines);
    let j = lines.iter().position(|l| l.starts_with("  • ")).expect(&format!("{:?}", lines));
    assert!(lines[j + 1].starts_with("    ") && !lines[j + 1].starts_with("     "), "bullet continuation hangs at 4: {:?}", lines);
}

/// write_envelope prints to STDERR while it runs; a spinner under it would be
/// left in scrollback with the stderr text glued on. So: no status line for
/// self-handled tools.
#[test]
fn no_spinner_for_self_handled_tools() {
    let r = Rig::new(W);
    r.w.begin_turn();
    r.w.print_tool_streaming_hint("write_envelope");
    r.w.tick();
    r.w.tick();
    let t = r.text();
    assert!(!t.contains("sealing") && !t.contains("thinking") && !t.contains('⠋'), "status drawn for a self-handled tool:\n{}", t);
    // a normal tool still gets one
    r.w.print_tool_streaming_hint("shell");
    r.w.tick();
    assert!(r.text().contains("running a command"), "{}", r.text());
}
