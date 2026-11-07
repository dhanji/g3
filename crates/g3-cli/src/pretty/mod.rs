//! Pretty terminal UI for g3 — Tokyo Night markdown, a typewriter with a
//! blinking cursor, and tool calls explained in plain words.
//!
//! ⚠️ ACTIVE ONLY ON A REAL TERMINAL. `choose()` decides once at startup; every
//! machine consumer (butler.app: piped + --quiet; duties: log files; studio;
//! scout, whose last line must be a path) gets the classic `ConsoleUiWriter`,
//! byte-for-byte unchanged. `G3_UI=classic` is the escape hatch.

pub mod explain;
pub mod screen;
pub mod theme;

use explain::{explain_tool, short, Explained};
use g3_core::ui_writer::UiWriter;
use screen::{Last, Screen, Status};
use theme::th;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SCREEN: OnceLock<Arc<Mutex<Screen>>> = OnceLock::new();

/// The whole selection rule, as a pure function (unit-tested truth table).
pub fn choose(stdout_is_tty: bool, quiet: bool, env_ui: Option<&str>) -> bool {
    if quiet {
        return false;
    }
    match env_ui.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "classic" | "plain" | "0" | "off" | "false") => false,
        _ => stdout_is_tty,
    }
}

/// True when this process is rendering with the pretty UI.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn with_screen(f: impl FnOnce(&mut Screen)) {
    if let Some(s) = SCREEN.get() {
        if let Ok(mut g) = s.lock() {
            f(&mut g);
            g.flush();
        }
    }
}

/// A user turn is starting: show the thinking spinner, start the clock.
pub fn begin_turn() {
    with_screen(|s| s.begin_turn());
}

/// Stop the spinner so a direct `println!` can't interleave with it.
pub fn suspend() {
    with_screen(|s| s.suspend());
}

/// Print a line through the screen (no-op when inactive — callers branch).
pub fn say(line: &str) {
    with_screen(|s| s.line(&format!("  {}", line), Last::Tool));
}

#[derive(Default)]
struct ToolRun {
    name: String,
    args: Vec<(String, String)>,
    ex: Option<Explained>,
    last_line: String,
    failed: bool,
}

struct State {
    tool: Option<ToolRun>,
    last_read_path: Option<String>,
    progress_msg: String,
    workspace: Option<PathBuf>,
}

pub struct PrettyUiWriter {
    screen: Arc<Mutex<Screen>>,
    state: Mutex<State>,
    stop: Arc<AtomicBool>,
}

/// A `Write` that tests can read back.
#[derive(Clone, Default)]
pub struct SharedBuf(pub Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SharedBuf {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl PrettyUiWriter {
    /// The real thing: stdout, a 60fps ticker, registered globally.
    pub fn new() -> Self {
        let width = crate::terminal_width::get_terminal_width();
        let screen = Arc::new(Mutex::new(Screen::new(Box::new(io::stdout()), width)));
        let _ = SCREEN.set(screen.clone());
        ACTIVE.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, stop2) = (screen.clone(), stop.clone());
        std::thread::Builder::new()
            .name("g3-pretty-ticker".into())
            .spawn(move || {
                let mut n = 0u64;
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(16));
                    n += 1;
                    if let Ok(mut s) = s2.lock() {
                        if n % 30 == 0 {
                            s.set_width(crate::terminal_width::get_terminal_width());
                        }
                        s.tick(Instant::now());
                    }
                }
            })
            .ok();
        Self { screen, state: Mutex::new(Self::fresh_state()), stop }
    }

    /// Test constructor: writes to `sink`, fixed width, no ticker, not global.
    pub fn with_sink(sink: SharedBuf, width: usize) -> Self {
        Self {
            screen: Arc::new(Mutex::new(Screen::new(Box::new(sink), width))),
            state: Mutex::new(Self::fresh_state()),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }

    fn fresh_state() -> State {
        State { tool: None, last_read_path: None, progress_msg: String::new(), workspace: None }
    }

    /// Advance one animation frame (tests drive this by hand).
    pub fn tick(&self) {
        self.screen.lock().unwrap().tick(Instant::now());
    }

    pub fn begin_turn(&self) {
        self.scr(|s| s.begin_turn());
    }

    pub fn pending_len(&self) -> usize {
        self.screen.lock().unwrap().pending_len()
    }

    fn scr<R>(&self, f: impl FnOnce(&mut Screen) -> R) -> R {
        let mut g = self.screen.lock().unwrap();
        let r = f(&mut g);
        g.flush();
        r
    }

    fn set_tool_status(&self, ex: Explained, started: Instant, preparing: bool) {
        self.scr(|s| {
            s.end_text();
            s.status = Status::Tool { ex, started, last_line: String::new(), preparing };
        });
    }

    fn arg(args: &[(String, String)], k: &str) -> Option<String> {
        args.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
    }

    fn range_suffix(args: &[(String, String)]) -> String {
        let (s, e) = (Self::arg(args, "start"), Self::arg(args, "end"));
        if s.is_none() && e.is_none() {
            return String::new();
        }
        format!(" [{}..{}]", s.unwrap_or_else(|| "0".into()), e.unwrap_or_else(|| "end".into()))
    }

    /// The collapsed "✓ 📄 read lib.rs  3ms" line.
    fn done_line(ex: &Explained, extra: &str, summary: &str, dur: &str, failed: bool, tail: &str) -> String {
        let mark = if failed { format!("{}✗", th().err.as_str()) } else { format!("{}✓", th().ok.as_str()) };
        let mut l = format!("  {} {}{} {}", mark, th().dim.as_str(), ex.icon, ex.done);
        if !ex.detail.is_empty() {
            l.push_str(&format!("  {}{}", th().sub.as_str(), ex.detail));
        }
        if !extra.is_empty() {
            l.push_str(&format!("{}{}", th().dim.as_str(), extra));
        }
        if !summary.is_empty() {
            l.push_str(&format!("  {}", summary));
        }
        if !dur.is_empty() {
            l.push_str(&format!("  {}{}", duration_color(dur), dur));
        }
        if failed && !tail.trim().is_empty() {
            l.push_str(&format!("  {}⟩ {}", th().err.as_str(), short(&screen::strip_ansi(tail), 60)));
        }
        l
    }

    fn count_tool(&self) {
        self.scr(|s| {
            if let Some(t) = s.turn.as_mut() {
                t.tools += 1;
            }
        });
    }

    fn after_tool(&self) {
        self.scr(|s| {
            s.status = if s.turn.is_some() { Status::Thinking { since: Instant::now() } } else { Status::None };
        });
    }
}

impl Drop for PrettyUiWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut s) = self.screen.lock() {
            s.suspend();
            s.flush();
        }
    }
}

/// Slow tools get warmer colours: ≥1s yellow, ≥1m orange, ≥5m red.
pub fn duration_color(d: &str) -> &'static str {
    if d.ends_with("ms") {
        return th().faint.as_str();
    }
    if let Some(m) = d.find('m') {
        if let Ok(min) = d[..m].trim().parse::<u32>() {
            return if min >= 5 { th().err.as_str() } else { th().slow.as_str() };
        }
    }
    if let Some(s) = d.strip_suffix('s') {
        if let Ok(sec) = s.trim().parse::<f64>() {
            if sec >= 1.0 {
                return th().warn.as_str();
            }
        }
    }
    th().faint.as_str()
}

/// "+5 | -3" → green +5, red −3.
pub fn colorize_edit_summary(s: &str) -> String {
    let parts: Vec<&str> = s.split(" | ").collect();
    let paint = |p: &str| {
        let p = p.trim();
        if p.starts_with('+') {
            format!("{}{}", th().ok.as_str(), p)
        } else if let Some(rest) = p.strip_prefix('-') {
            format!("{}−{}", th().err.as_str(), rest)
        } else {
            format!("{}{}", th().dim.as_str(), p)
        }
    };
    parts.iter().map(|p| paint(p)).collect::<Vec<_>>().join(" ")
}

fn ctx_color(pct: f32) -> &'static str {
    if pct < 40.0 {
        th().ok.as_str()
    } else if pct < 60.0 {
        th().warn.as_str()
    } else if pct < 80.0 {
        th().slow.as_str()
    } else {
        th().err.as_str()
    }
}

pub fn compact_tokens(t: u32) -> String {
    if t >= 1_000_000 {
        format!("{:.1}m", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        let k = t as f64 / 1000.0;
        if k >= 100.0 { format!("{:.0}k", k) } else { format!("{:.1}k", k) }
    } else {
        t.to_string()
    }
}

pub fn footer_line(elapsed: Duration, tools: usize, used: u32, total: u32, pct: f32) -> String {
    let secs = elapsed.as_secs_f64();
    let time = if secs >= 60.0 { format!("{}m {:.0}s", (secs / 60.0) as u64, secs % 60.0) } else { format!("{:.1}s", secs) };
    let mut bits = vec![time];
    if tools > 0 {
        bits.push(format!("{} tool{}", tools, if tools == 1 { "" } else { "s" }));
    }
    let sep = format!("{} · ", th().faint.as_str());
    let mut l = format!("  {}─ {}", th().faint.as_str(), bits.join(&sep));
    l.push_str(&format!("{}{}ctx {:.0}%{} {}/{}", sep, ctx_color(pct), pct, th().faint.as_str(), compact_tokens(used), compact_tokens(total)));
    l
}

const COMPACT_TOOLS: &[&str] = &[
    "read_file", "write_file", "str_replace", "remember", "screenshot", "coverage", "code_search",
    "plan_approve", "load_toolset",
];

impl UiWriter for PrettyUiWriter {
    fn print(&self, message: &str) {
        self.scr(|s| s.raw(message));
    }

    fn println(&self, message: &str) {
        if message.is_empty() {
            return; // the screen owns spacing
        }
        let msg = message.trim_end_matches('\n').to_string();
        self.scr(|s| {
            for l in msg.lines() {
                s.line(&format!("  {}{}", th().sub.as_str(), l), Last::Tool);
            }
        });
    }

    fn print_inline(&self, message: &str) {
        self.scr(|s| s.raw(message));
    }

    fn print_system_prompt(&self, prompt: &str) {
        self.scr(|s| {
            s.raw(&format!("🔍 System Prompt:\n================\n{}\n================\n\n", prompt));
        });
    }

    fn print_context_status(&self, message: &str) {
        let m = message.trim().to_string();
        if m.is_empty() {
            return;
        }
        self.scr(|s| s.line(&format!("  {}◦ {}", th().dim.as_str(), m), Last::Tool));
    }

    fn print_g3_progress(&self, message: &str) {
        self.state.lock().unwrap().progress_msg = message.to_string();
        let msg = message.to_string();
        self.scr(|s| {
            s.end_text();
            s.status = Status::Progress { msg, since: Instant::now() };
        });
    }

    fn print_g3_status(&self, message: &str, status: &str) {
        let msg = {
            let st = self.state.lock().unwrap();
            if message.is_empty() { st.progress_msg.clone() } else { message.to_string() }
        };
        let st = status.trim();
        let (mark, col) = if matches!(st, "done" | "resolved") {
            ("✓", th().ok.as_str())
        } else if st.starts_with("fail") || st.starts_with("error") || st == "insufficient" {
            ("✗", th().err.as_str())
        } else {
            ("◦", th().dim.as_str())
        };
        let tail = if matches!(st, "done") { String::new() } else { format!("  {}{}", th().dim.as_str(), st) };
        self.scr(|s| {
            s.line(&format!("  {}{} {}{}{}", col, mark, th().dim.as_str(), msg, tail), Last::Tool);
            s.status = Status::None;
            s.thinking();
        });
    }

    fn print_thin_result(&self, r: &g3_core::ThinResult) {
        let what = match r.scope {
            g3_core::ThinScope::FirstThird => "thinned context",
            g3_core::ThinScope::All => "thinned context (full)",
        };
        let l = if r.had_changes {
            format!("  {}🥒 {}{}  {}% → {}%", th().ok.as_str(), th().dim.as_str(), what, r.before_percentage, r.after_percentage)
        } else {
            format!("  {}🥒 {} — no changes  {}%", th().dim.as_str(), what, r.before_percentage)
        };
        self.scr(|s| s.line(&l, Last::Tool));
    }

    fn print_context_thinning(&self, message: &str) {
        self.print_context_status(message);
    }

    fn print_context_summary(&self, used_tokens: u32, total_tokens: u32, percentage: f32) {
        self.scr(|s| {
            s.end_text();
            s.clear_status();
            s.status = Status::None;
            if let Some(turn) = s.turn.take() {
                let l = footer_line(turn.start.elapsed(), turn.tools, used_tokens, total_tokens, percentage);
                s.line(&l, Last::Block);
                s.line("", Last::Nothing);
            }
        });
    }

    fn print_tool_header(&self, tool_name: &str, tool_args: Option<&serde_json::Value>) {
        let ex = explain_tool(tool_name, tool_args.unwrap_or(&serde_json::Value::Null));
        let now = Instant::now();
        {
            let mut st = self.state.lock().unwrap();
            st.tool = Some(ToolRun { name: tool_name.to_string(), ex: Some(ex.clone()), ..Default::default() });
        }
        self.set_tool_status(ex, now, false);
    }

    fn print_tool_arg(&self, key: &str, value: &str) {
        if let Some(t) = self.state.lock().unwrap().tool.as_mut() {
            t.args.push((key.to_string(), value.to_string()));
        }
    }

    fn print_tool_output_header(&self) {}

    fn update_tool_output_line(&self, line: &str) {
        {
            let mut st = self.state.lock().unwrap();
            if let Some(t) = st.tool.as_mut() {
                t.last_line = line.to_string();
                if line.contains('❌') {
                    t.failed = true;
                }
            }
        }
        let l = line.to_string();
        self.scr(|s| {
            if let Status::Tool { last_line, .. } = &mut s.status {
                *last_line = l;
            }
        });
    }

    fn print_tool_output_line(&self, line: &str) {
        self.update_tool_output_line(line);
    }

    fn print_tool_output_summary(&self, _hidden_count: usize) {}

    fn print_tool_compact(&self, tool_name: &str, summary: &str, duration_str: &str, _tokens_delta: u32, _ctx: f32) -> bool {
        if !COMPACT_TOOLS.contains(&tool_name) {
            self.state.lock().unwrap().last_read_path = None;
            return false;
        }
        let (run, cont) = {
            let mut st = self.state.lock().unwrap();
            let run = st.tool.take().unwrap_or_default();
            let path = Self::arg(&run.args, "file_path").unwrap_or_default();
            let cont = tool_name == "read_file" && !path.is_empty() && st.last_read_path.as_deref() == Some(path.as_str());
            st.last_read_path = if tool_name == "read_file" && !path.is_empty() { Some(path) } else { None };
            (run, cont)
        };
        let ex = run.ex.clone().unwrap_or_else(|| explain_tool(tool_name, &serde_json::Value::Null));
        let extra = if tool_name == "read_file" { Self::range_suffix(&run.args) } else { String::new() };
        let failed = summary.contains('❌') || run.failed;
        let summ = if tool_name == "str_replace" {
            colorize_edit_summary(summary)
        } else {
            format!("{}{}", th().dim.as_str(), short(summary, 50))
        };
        let line = if cont {
            format!("    {}└ read further{}  {}  {}{}", th().dim.as_str(), extra, summ, duration_color(duration_str), duration_str)
        } else {
            Self::done_line(&ex, &extra, &summ, duration_str, failed, summary)
        };
        self.scr(|s| s.line(&line, Last::Tool));
        self.count_tool();
        self.after_tool();
        true
    }

    fn print_todo_compact(&self, content: Option<&str>, is_write: bool) -> bool {
        self.state.lock().unwrap().last_read_path = None;
        let label = if is_write { "updated todos" } else { "todos" };
        let mut lines = vec![format!("  {}📌 {}{}", th().accent.as_str(), th().fg.as_str(), label)];
        match content {
            None => lines[0].push_str(&format!("  {}empty", th().dim.as_str())),
            Some(text) => {
                for l in text.lines().filter(|l| !l.trim().is_empty()) {
                    let done = l.contains("- [x]") || l.contains("- [X]");
                    let body = l.replace("- [x]", "").replace("- [X]", "").replace("- [ ]", "");
                    let body = body.trim();
                    if done {
                        lines.push(format!("    {}■ {}{}{}", th().ok.as_str(), th().dim.as_str(), th().strike, body));
                    } else if l.contains("- [ ]") {
                        lines.push(format!("    {}□ {}{}", th().dim.as_str(), th().fg.as_str(), body));
                    } else {
                        lines.push(format!("    {}{}", th().dim.as_str(), body));
                    }
                }
            }
        }
        self.scr(|s| {
            for l in &lines {
                s.line(l, Last::Block);
            }
        });
        self.after_tool();
        true
    }

    fn print_plan_compact(&self, plan_yaml: Option<&str>, plan_file_path: Option<&str>, is_write: bool) -> bool {
        self.state.lock().unwrap().last_read_path = None;
        let lines = render_plan(plan_yaml, plan_file_path, is_write);
        self.scr(|s| {
            for l in &lines {
                s.line(l, Last::Block);
            }
        });
        self.after_tool();
        true
    }

    fn print_envelope_compact(&self, fact_groups: usize, stages: &[(&str, &str)], passed: Option<usize>, total: Option<usize>, failed: usize) {
        let mut lines = vec![format!(
            "  {}📨 {}envelope  {}{} fact group{}",
            th().accent.as_str(), th().fg.as_str(), th().dim.as_str(), fact_groups, if fact_groups == 1 { "" } else { "s" }
        )];
        let has_v = passed.is_some();
        for (i, (icon, desc)) in stages.iter().enumerate() {
            let last = i + 1 == stages.len() && !has_v;
            lines.push(format!("    {}{} {} {}", th().faint.as_str(), if last { "└" } else { "├" }, icon, th().dim.as_str().to_string() + desc));
        }
        if let (Some(p), Some(t)) = (passed, total) {
            if failed == 0 {
                lines.push(format!("    {}└ {}✓ {}/{} passed", th().faint.as_str(), th().ok.as_str(), p, t));
            } else {
                lines.push(format!("    {}└ {}✗ {}/{} passed, {} failed", th().faint.as_str(), th().err.as_str(), p, t, failed));
            }
        }
        self.scr(|s| {
            for l in &lines {
                s.line(l, Last::Block);
            }
        });
        self.after_tool();
    }

    fn print_tool_timing(&self, duration_str: &str, _tokens_delta: u32, _ctx: f32) {
        let run = {
            let mut st = self.state.lock().unwrap();
            let run = st.tool.take().unwrap_or_default();
            if run.name != "read_file" {
                st.last_read_path = None;
            }
            run
        };
        let ex = run.ex.clone().unwrap_or_else(|| explain_tool(&run.name, &serde_json::Value::Null));
        let line = Self::done_line(&ex, "", "", duration_str, run.failed, &run.last_line);
        self.scr(|s| s.line(&line, Last::Tool));
        self.count_tool();
        self.after_tool();
    }

    fn print_agent_prompt(&self) {
        self.scr(|s| s.thinking());
    }

    fn print_agent_response(&self, content: &str) {
        if content.is_empty() {
            return;
        }
        self.state.lock().unwrap().last_read_path = None;
        self.scr(|s| s.feed_text(content));
    }

    fn notify_sse_received(&self) {}

    fn print_tool_streaming_hint(&self, tool_name: &str) {
        // Self-handled tools (plan/todo/envelope) are fast and some write to
        // STDERR while running (envelope verification). A spinner drawn under
        // that would be left in scrollback with the stderr text glued on, so
        // for them: no status line at all.
        if matches!(tool_name, "todo_read" | "todo_write" | "plan_read" | "plan_write" | "write_envelope") {
            self.scr(|s| s.suspend());
            return;
        }
        let has_tool = self.state.lock().unwrap().tool.is_some();
        if !has_tool {
            self.set_tool_status(explain_tool(tool_name, &serde_json::Value::Null), Instant::now(), true);
        }
    }

    fn print_tool_streaming_active(&self) {}

    fn flush(&self) {
        self.scr(|_| {});
    }

    fn prompt_user_yes_no(&self, message: &str) -> bool {
        self.scr(|s| s.suspend());
        print!("  {}?{} {} {}[y/N]{} ", th().accent.as_str(), th().reset, message, th().dim.as_str(), th().reset);
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let t = input.trim().to_lowercase();
            t == "y" || t == "yes"
        } else {
            false
        }
    }

    fn prompt_user_choice(&self, message: &str, options: &[&str]) -> usize {
        self.scr(|s| s.suspend());
        println!("  {}?{} {}", th().accent.as_str(), th().reset, message);
        for (i, o) in options.iter().enumerate() {
            println!("    {}{}{} {}", th().accent.as_str(), i + 1, th().reset, o);
        }
        loop {
            print!("  {}select 1–{}:{} ", th().dim.as_str(), options.len(), th().reset);
            let _ = io::stdout().flush();
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                if let Ok(c) = input.trim().parse::<usize>() {
                    if c > 0 && c <= options.len() {
                        return c - 1;
                    }
                }
            }
        }
    }

    // Same filter as classic — display-only, but it decides what text reaches
    // the screen, so it must not diverge.
    fn filter_json_tool_calls(&self, content: &str) -> String {
        crate::filter_json::filter_json_tool_calls(content)
    }

    fn reset_json_filter(&self) {
        crate::filter_json::reset_json_tool_state();
    }

    fn finish_streaming_markdown(&self) {
        self.scr(|s| s.end_text());
    }

    fn set_workspace_path(&self, path: PathBuf) {
        self.state.lock().unwrap().workspace = Some(path);
    }
}

/// Plan tree in Tokyo Night. Mirrors the classic content; only the look differs.
pub fn render_plan(plan_yaml: Option<&str>, plan_file_path: Option<&str>, is_write: bool) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Plan {
        plan_id: String,
        approved_revision: Option<u32>,
        items: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        id: String,
        description: String,
        state: String,
        #[serde(default)]
        touches: Vec<String>,
        #[serde(default)]
        checks: Option<Checks>,
        #[serde(default)]
        evidence: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct Checks {
        happy: Check,
        #[serde(default)]
        negative: Vec<Check>,
        #[serde(default)]
        boundary: Vec<Check>,
    }
    #[derive(serde::Deserialize)]
    struct Check {
        desc: String,
    }

    let verb = if is_write { "plan updated" } else { "plan" };
    let mut out = Vec::new();
    let Some(yaml) = plan_yaml else {
        out.push(format!("  {}📐 {}{}  {}empty", th().accent.as_str(), th().fg.as_str(), verb, th().dim.as_str()));
        return out;
    };
    let Ok(plan) = serde_yaml::from_str::<Plan>(yaml) else {
        out.push(format!("  {}📐 {}{}", th().accent.as_str(), th().fg.as_str(), verb));
        for l in yaml.lines().take(20) {
            out.push(format!("    {}{}", th().dim.as_str(), l));
        }
        return out;
    };
    let count = |s: &str| plan.items.iter().filter(|i| i.state == s).count();
    let (done, doing, blocked, todo) = (count("done"), count("doing"), count("blocked"), count("todo"));
    let approved = match plan.approved_revision {
        Some(r) => format!("{}✓ approved@{}", th().ok.as_str(), r),
        None => format!("{}⚠ not approved", th().warn.as_str()),
    };
    let bar = format!(
        "{}{}{}{}{}{}{}{}",
        th().ok.as_str(), "■".repeat(done), th().warn.as_str(), "■".repeat(doing), th().err.as_str(), "■".repeat(blocked), th().faint.as_str(), "□".repeat(todo)
    );
    out.push(format!(
        "  {}📐 {}{}  {}{}{}  {}  {}{}/{}  {}",
        th().accent.as_str(), th().fg.as_str(), verb, th().bold, th().info.as_str(), plan.plan_id, approved, th().dim.as_str(), done, plan.items.len(), bar
    ));
    let n = plan.items.len();
    for (i, it) in plan.items.iter().enumerate() {
        let last = i + 1 == n;
        let (icon, col) = match it.state.as_str() {
            "doing" => ("◐", th().warn.as_str()),
            "done" => ("■", th().ok.as_str()),
            "blocked" => ("⊘", th().err.as_str()),
            _ => ("□", th().dim.as_str()),
        };
        let branch = if last { "└" } else { "├" };
        let child = if last { " " } else { "│" };
        let desc = if it.state == "done" {
            format!("{}{}{}", th().dim.as_str(), th().strike, it.description)
        } else {
            format!("{}{}", th().fg.as_str(), it.description)
        };
        out.push(format!("    {}{} {}{} {}{}{}{} {}", th().faint.as_str(), branch, col, icon, th().reset, th().bold, th().sub.as_str(), it.id, desc));
        if it.state == "done" {
            if !it.evidence.is_empty() {
                out.push(format!("    {}{}    📎 {}", th().faint.as_str(), child, it.evidence.join(", ")));
            }
        } else {
            if !it.touches.is_empty() {
                out.push(format!("    {}{}    {}→ {}", th().faint.as_str(), child, th().dim.as_str(), it.touches.join(", ")));
            }
            if let Some(c) = &it.checks {
                out.push(format!("    {}{}    {}✓ {}{}", th().faint.as_str(), child, th().ok.as_str(), th().dim.as_str(), c.happy.desc));
                for x in &c.negative {
                    out.push(format!("    {}{}    {}✗ {}{}", th().faint.as_str(), child, th().err.as_str(), th().dim.as_str(), x.desc));
                }
                for x in &c.boundary {
                    out.push(format!("    {}{}    {}◇ {}{}", th().faint.as_str(), child, th().warn.as_str(), th().dim.as_str(), x.desc));
                }
            }
        }
    }
    if let Some(p) = plan_file_path {
        out.push(format!("    {}📄 {}", th().faint.as_str(), p));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_truth_table() {
        // (tty, quiet, env) → pretty?
        let cases: &[(bool, bool, Option<&str>, bool)] = &[
            (true, false, None, true),
            (false, false, None, false),          // piped (butler.app, duties, scout)
            (true, true, None, false),            // --quiet
            (false, true, None, false),           // butler.app exactly
            (true, false, Some("classic"), false),
            (true, false, Some("CLASSIC"), false),
            (true, false, Some("0"), false),
            (true, false, Some("pretty"), true),
            (false, false, Some("pretty"), false), // never forced onto a pipe
            (true, false, Some(""), true),
        ];
        for (tty, quiet, env, want) in cases {
            assert_eq!(choose(*tty, *quiet, *env), *want, "tty={} quiet={} env={:?}", tty, quiet, env);
        }
    }

    #[test]
    fn edit_summary_colours() {
        let s = screen::strip_ansi(&colorize_edit_summary("+5 | -3"));
        assert_eq!(s, "+5 −3");
        assert!(colorize_edit_summary("+5 | -3").contains(th().ok.as_str()));
        assert!(colorize_edit_summary("+5 | -3").contains(th().err.as_str()));
    }

    #[test]
    fn duration_colours_match_classic_thresholds() {
        assert_eq!(duration_color("132ms"), th().faint.as_str());
        assert_eq!(duration_color("0.9s"), th().faint.as_str());
        assert_eq!(duration_color("1.2s"), th().warn.as_str());
        assert_eq!(duration_color("2m 3.0s"), th().slow.as_str());
        assert_eq!(duration_color("7m 0.0s"), th().err.as_str());
    }

    #[test]
    fn footer_ctx_thresholds() {
        let f = |p| footer_line(Duration::from_millis(12_300), 3, 38_500, 1_000_000, p);
        assert!(f(10.0).contains(th().ok.as_str()));
        assert!(f(45.0).contains(th().warn.as_str()));
        assert!(f(65.0).contains(th().slow.as_str()));
        assert!(f(85.0).contains(th().err.as_str()));
        assert_eq!(screen::strip_ansi(&f(4.2)), "  ─ 12.3s · 3 tools · ctx 4% 38.5k/1.0m");
    }
}
