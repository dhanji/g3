//! The terminal state machine behind pretty mode.
//!
//! One `Screen` owns everything that is drawn: streamed prose (typewriter +
//! word-wrap + 2-col gutter + blinking cursor), a single transient status line
//! (spinner / running tool), and permanent lines (tool summaries, footer).
//!
//! Geometry invariants — every bug in this file would be a violation of one:
//!   * the status line is only ever drawn when the cursor is at column 0 of an
//!     otherwise-empty line, so `\r\x1b[2K` always erases exactly it;
//!   * the cursor glyph is only drawn when `col < width - 1`, so drawing it can
//!     never trigger a terminal wrap;
//!   * every permanent write first erases the cursor and the status line.
//!
//! Writes go to an injectable sink so tests can feed the bytes to a terminal
//! emulator and assert on what a person would actually see.

use crate::streaming_markdown::StreamingMarkdownFormatter;
use std::collections::VecDeque;
use std::io::Write;
use std::time::Instant;
use termimad::crossterm::style::Color;
use termimad::MadSkin;
use unicode_width::UnicodeWidthChar;

use super::explain::Explained;

/// Tokyo Night, as ANSI 24-bit foreground prefixes.
pub mod tn {
    pub const FG: &str = "\x1b[38;2;192;202;245m";
    pub const SUB: &str = "\x1b[38;2;154;165;206m";
    pub const DIM: &str = "\x1b[38;2;86;95;137m";
    pub const FAINT: &str = "\x1b[38;2;59;66;97m";
    pub const BLUE: &str = "\x1b[38;2;122;162;247m";
    pub const PURPLE: &str = "\x1b[38;2;187;154;247m";
    pub const CYAN: &str = "\x1b[38;2;125;207;255m";
    pub const GREEN: &str = "\x1b[38;2;158;206;106m";
    pub const ORANGE: &str = "\x1b[38;2;255;158;100m";
    pub const RED: &str = "\x1b[38;2;247;118;142m";
    pub const YELLOW: &str = "\x1b[38;2;224;175;104m";
    pub const CODE_BG: &str = "\x1b[48;2;41;46;66m";
    pub const BOLD: &str = "\x1b[1m";
    pub const ITALIC: &str = "\x1b[3m";
    pub const STRIKE: &str = "\x1b[9m";
    pub const RESET: &str = "\x1b[0m";
}

pub const GUTTER: usize = 2;
const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Catppuccin (what StreamingMarkdownFormatter emits) → Tokyo Night.
/// Remapping on the way OUT keeps the formatter, and therefore classic mode,
/// byte-identical. Headings gain a gutter mark; each source colour appears in
/// exactly one construct in the formatter (checked by a test against its source).
const REMAP: &[(&str, &str)] = &[
    // H1: bold mauve
    ("\x1b[1m\x1b[38;2;198;160;246m", "\x1b[1m\x1b[38;2;122;162;247m▌ "),
    // H2: blue
    ("\x1b[38;2;138;173;244m", "\x1b[1m\x1b[38;2;187;154;247m▍ "),
    // H3: lavender
    ("\x1b[38;2;183;189;248m", "\x1b[1m\x1b[38;2;125;207;255m▏ "),
    // H4: teal
    ("\x1b[38;2;139;213;202m", "\x1b[1m\x1b[38;2;158;206;106m"),
    // H5/6 + muted: subtext1
    ("\x1b[38;2;165;173;203m", "\x1b[38;2;154;165;206m"),
    // italic: sky
    ("\x1b[38;2;145;215;227m", "\x1b[38;2;192;202;245m"),
    // bold: sapphire
    ("\x1b[38;2;125;196;228m", "\x1b[38;2;192;202;245m"),
    // inline code: peach → orange on a soft chip
    ("\x1b[38;2;245;169;127m", "\x1b[38;2;255;158;100m\x1b[48;2;41;46;66m"),
    // links: green → cyan
    ("\x1b[38;2;166;218;149m", "\x1b[38;2;125;207;255m"),
    // rules + unhighlighted code: overlay1
    ("\x1b[38;2;128;135;162m", "\x1b[38;2;86;95;137m"),
];

pub fn remap(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = s.to_string();
    for (from, to) in REMAP {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    out
}

pub fn tn_skin() -> MadSkin {
    let rgb = |r, g, b| Color::Rgb { r, g, b };
    let mut skin = MadSkin::default();
    skin.bold.set_fg(rgb(192, 202, 245));
    skin.italic.set_fg(rgb(192, 202, 245));
    skin.inline_code.set_fg(rgb(255, 158, 100));
    skin.inline_code.set_bg(rgb(41, 46, 66));
    skin.code_block.set_fg(rgb(192, 202, 245));
    skin.table.set_fg(rgb(59, 66, 97));
    skin.quote_mark.set_fg(rgb(187, 154, 247));
    skin.bullet.set_fg(rgb(187, 154, 247));
    skin.horizontal_rule.set_fg(rgb(59, 66, 97));
    for h in skin.headers.iter_mut() {
        h.set_fg(rgb(122, 162, 247));
    }
    skin
}

/// Visible width of a string, skipping ANSI CSI/OSC sequences.
pub fn visible_width(s: &str) -> usize {
    let mut w = 0;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            skip_escape(&mut it);
        } else {
            w += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    w
}

fn skip_escape<I: Iterator<Item = char>>(it: &mut std::iter::Peekable<I>) -> String {
    let mut seq = String::from('\x1b');
    match it.next() {
        Some('[') => {
            seq.push('[');
            for c in it.by_ref() {
                seq.push(c);
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
        Some(']') => {
            seq.push(']');
            while let Some(c) = it.next() {
                seq.push(c);
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' {
                    if let Some(n) = it.next() {
                        seq.push(n);
                    }
                    break;
                }
            }
        }
        Some(c) => seq.push(c),
        None => {}
    }
    seq
}

/// Clip to `max` visible columns (ANSI-aware), appending … and a reset when cut.
pub fn clip_ansi(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            out.push_str(&skip_escape(&mut it));
            continue;
        }
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw + 1 > max {
            out.push('…');
            out.push_str(tn::RESET);
            return out;
        }
        out.push(c);
        w += cw;
    }
    out
}

#[derive(Debug, Clone)]
pub enum Status {
    None,
    Thinking { since: Instant },
    Tool { ex: Explained, started: Instant, last_line: String, preparing: bool },
    Progress { msg: String, since: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    Nothing,
    Text,
    Tool,
    Block,
}

pub struct Turn {
    pub start: Instant,
    pub tools: usize,
}

pub struct Screen {
    out: Box<dyn Write + Send>,
    pub width: usize,
    // ── prose ──
    pending: VecDeque<char>,
    md: Option<StreamingMarkdownFormatter>,
    pub streaming: bool,
    col: usize,
    word: String,
    word_w: usize,
    /// Visible text of the current output line (for hanging indents).
    line_text: String,
    at_line_start: bool,
    blank_lines: usize,
    cursor_on: bool,
    // ── status ──
    pub status: Status,
    status_drawn: bool,
    pub turn: Option<Turn>,
    pub last: Last,
    frame: u64,
    born: Instant,
}

/// Reveal at least this many chars per frame (≈180 cps at 60fps)…
pub const MIN_STEP: usize = 3;
/// …and 1/CATCHUP of the backlog, so it never lags far behind the model.
pub const CATCHUP: usize = 8;

impl Screen {
    pub fn new(out: Box<dyn Write + Send>, width: usize) -> Self {
        Self {
            out,
            width: width.max(20),
            pending: VecDeque::new(),
            md: None,
            streaming: false,
            col: 0,
            word: String::new(),
            word_w: 0,
            line_text: String::new(),
            at_line_start: true,
            blank_lines: 1,
            cursor_on: false,
            status: Status::None,
            status_drawn: false,
            turn: None,
            last: Last::Nothing,
            frame: 0,
            born: Instant::now(),
        }
    }

    fn w(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
    }

    pub fn flush(&mut self) {
        let _ = self.out.flush();
    }

    pub fn set_width(&mut self, w: usize) {
        self.width = w.max(20);
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    // ── cursor + status primitives ──────────────────────────────────────
    fn erase_cursor(&mut self) {
        if self.cursor_on {
            self.w(" \x1b[D");
            self.cursor_on = false;
        }
    }

    fn draw_cursor(&mut self) {
        if !self.cursor_on && self.col < self.width.saturating_sub(1) {
            self.w(&format!("{}▌{}\x1b[D", tn::PURPLE, tn::RESET));
            self.cursor_on = true;
        }
    }

    pub fn clear_status(&mut self) {
        if self.status_drawn {
            self.w("\r\x1b[2K");
            self.status_drawn = false;
        }
    }

    fn newline(&mut self) {
        self.erase_cursor();
        if self.at_line_start && self.col == 0 {
            self.blank_lines += 1;
        } else {
            self.blank_lines = 0;
        }
        self.w("\r\n");
        self.col = 0;
        self.at_line_start = true;
        self.line_text.clear();
    }

    /// Ensure the cursor sits at column 0 of a fresh line.
    fn to_line_start(&mut self) {
        if !self.at_line_start || self.col != 0 {
            self.newline();
        }
    }

    /// One blank line of separation, unless there already is one.
    fn separate(&mut self) {
        self.to_line_start();
        if self.blank_lines == 0 {
            self.newline();
        }
    }

    // ── prose ───────────────────────────────────────────────────────────
    pub fn feed_text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if !self.streaming {
            self.begin_text();
        }
        self.pending.extend(s.chars());
    }

    fn begin_text(&mut self) {
        self.clear_status();
        if matches!(self.last, Last::Tool | Last::Block) {
            self.separate();
        } else {
            self.to_line_start();
        }
        self.streaming = true;
        self.md = Some(StreamingMarkdownFormatter::new(tn_skin()));
        self.status = Status::None;
    }

    fn reveal(&mut self, n: usize) {
        let take = n.min(self.pending.len());
        if take == 0 {
            return;
        }
        let chunk: String = self.pending.drain(..take).collect();
        let formatted = match self.md.as_mut() {
            Some(md) => md.process(&chunk),
            None => chunk,
        };
        self.emit(&remap(&formatted));
    }

    /// Write formatted (ANSI) text with word-wrap and the gutter.
    fn emit(&mut self, s: &str) {
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => {
                    let seq = skip_escape(&mut it);
                    self.word.push_str(&seq);
                }
                '\n' => {
                    self.flush_word();
                    self.newline();
                }
                '\r' => {}
                ' ' => {
                    self.flush_word();
                    self.put_space();
                }
                '\t' => {
                    self.flush_word();
                    for _ in 0..4 {
                        self.put_space();
                    }
                }
                _ => {
                    self.word.push(c);
                    self.word_w += UnicodeWidthChar::width(c).unwrap_or(0);
                    if self.word_w >= self.width.saturating_sub(GUTTER + 1) {
                        self.flush_word(); // a word wider than the screen: hard break
                    }
                }
            }
        }
    }

    fn start_line(&mut self) {
        if self.at_line_start {
            self.w(&" ".repeat(GUTTER));
            self.col = GUTTER;
            self.at_line_start = false;
        }
    }

    fn flush_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        self.erase_cursor();
        let word = std::mem::take(&mut self.word);
        let ww = std::mem::replace(&mut self.word_w, 0);
        if ww == 0 {
            self.w(&word); // escapes only: no gutter, no width
            return;
        }
        self.start_line();
        if self.col + ww > self.width.saturating_sub(1) && self.col > GUTTER {
            self.wrap();
        }
        self.w(&word);
        self.col += ww;
        self.line_text.push_str(&strip_ansi(&word));
        self.blank_lines = 0;
    }

    /// Soft-wrap: continue on a new line, indented under the text of a list
    /// item / quote rather than under its marker.
    fn wrap(&mut self) {
        let hang = hanging_indent(&self.line_text);
        self.w("\r\n");
        self.w(&" ".repeat(GUTTER + hang));
        self.col = GUTTER + hang;
        self.at_line_start = false;
        // keep the marker context so a third line hangs the same way
        self.line_text = " ".repeat(hang);
    }

    fn put_space(&mut self) {
        self.erase_cursor();
        self.start_line();
        if self.col >= self.width.saturating_sub(1) {
            self.wrap();
        } else {
            self.w(" ");
            self.col += 1;
            self.line_text.push(' ');
        }
    }

    /// Stop streaming prose: reveal everything, flush the formatter, park the
    /// cursor at the start of a fresh line. Synchronous, so nothing is ever
    /// lost or reordered behind a tool line.
    pub fn end_text(&mut self) {
        if !self.streaming {
            return;
        }
        let rest: String = self.pending.drain(..).collect();
        let mut formatted = String::new();
        if let Some(md) = self.md.as_mut() {
            formatted.push_str(&md.process(&rest));
            formatted.push_str(&md.finish());
        } else {
            formatted.push_str(&rest);
        }
        self.emit(&remap(&formatted));
        self.flush_word();
        self.erase_cursor();
        self.w(tn::RESET);
        self.to_line_start();
        self.streaming = false;
        self.md = None;
        self.last = Last::Text;
    }

    // ── permanent lines ─────────────────────────────────────────────────
    /// Print a permanent line (already styled). `kind` drives spacing.
    pub fn line(&mut self, s: &str, kind: Last) {
        self.end_text();
        self.clear_status();
        if self.last == Last::Text && kind != Last::Text {
            self.separate();
        } else if kind == Last::Block && self.last == Last::Tool {
            self.separate();
        } else {
            self.to_line_start();
        }
        let clipped = clip_ansi(s, self.width.saturating_sub(1));
        self.w(&clipped);
        self.w(tn::RESET);
        self.w("\r\n");
        self.col = 0;
        self.at_line_start = true;
        self.blank_lines = if visible_width(s) == 0 { self.blank_lines + 1 } else { 0 };
        self.last = kind;
    }

    /// Raw passthrough for messages we don't own the shape of.
    pub fn raw(&mut self, s: &str) {
        self.end_text();
        self.clear_status();
        self.w(s);
        if let Some(tail) = s.rsplit('\n').next() {
            if s.contains('\n') {
                self.col = visible_width(tail);
            } else {
                self.col += visible_width(tail);
            }
        }
        self.at_line_start = self.col == 0;
        if !s.trim().is_empty() {
            self.blank_lines = 0;
        }
    }

    // ── turn + status ───────────────────────────────────────────────────
    pub fn begin_turn(&mut self) {
        let now = Instant::now();
        self.turn = Some(Turn { start: now, tools: 0 });
        self.status = Status::Thinking { since: now };
        self.last = Last::Nothing;
        self.blank_lines = 1;
    }

    /// Stop drawing the status line (keeps turn stats for the footer).
    pub fn suspend(&mut self) {
        self.end_text();
        self.clear_status();
        self.status = Status::None;
    }

    pub fn thinking(&mut self) {
        if self.turn.is_some() && !matches!(self.status, Status::Thinking { .. } | Status::Tool { .. }) {
            self.status = Status::Thinking { since: Instant::now() };
        }
    }

    pub fn status_text(&self, now: Instant) -> Option<String> {
        let spin = SPIN[((now - self.born).as_millis() / 80) as usize % SPIN.len()];
        let mut t = String::new();
        match &self.status {
            Status::None => return None,
            Status::Thinking { since } => {
                if self.turn.is_none() {
                    return None;
                }
                let el = (now - *since).as_secs();
                t.push_str(&format!("  {}{} {}thinking", tn::PURPLE, spin, tn::SUB));
                let total = self.turn.as_ref().map(|tr| (now - tr.start).as_secs()).unwrap_or(el);
                if total >= 3 {
                    t.push_str(&format!("{}  {}s", tn::DIM, total));
                }
            }
            Status::Tool { ex, started, last_line, preparing } => {
                t.push_str(&format!("  {}{} {}{} {}", tn::CYAN, spin, tn::FG, ex.icon, ex.doing));
                if !ex.detail.is_empty() {
                    t.push_str(&format!("  {}{}", tn::SUB, ex.detail));
                }
                let el = (now - *started).as_secs();
                if *preparing {
                    t.push_str(&format!("{}  …", tn::DIM));
                } else if el >= 2 {
                    t.push_str(&format!("{}  {}s", tn::DIM, el));
                }
                if !last_line.trim().is_empty() {
                    let used = visible_width(&t) + 6;
                    let room = self.width.saturating_sub(used).max(8);
                    let tail = super::explain::short(&strip_ansi(last_line), room);
                    t.push_str(&format!("  {}⟩ {}", tn::FAINT, tail));
                }
            }
            Status::Progress { msg, .. } => {
                t.push_str(&format!("  {}{} {}{}", tn::PURPLE, spin, tn::SUB, msg));
            }
        }
        Some(t)
    }

    fn draw_status(&mut self, now: Instant) {
        match self.status_text(now) {
            None => self.clear_status(),
            Some(text) => {
                self.to_line_start();
                let clipped = clip_ansi(&text, self.width.saturating_sub(1));
                self.w(&format!("\r\x1b[2K{}{}", clipped, tn::RESET));
                self.status_drawn = true;
            }
        }
    }

    /// One animation frame. Called ~60×/s by the ticker (or by tests).
    pub fn tick(&mut self, now: Instant) {
        self.frame += 1;
        if self.streaming {
            let n = MIN_STEP.max(self.pending.len() / CATCHUP);
            self.reveal(n);
            if self.pending.is_empty() {
                // caught up with the model: show the partial word rather than hide it
                self.flush_word();
                let blink_on = ((now - self.born).as_millis() / 530) % 2 == 0;
                if blink_on {
                    self.draw_cursor();
                } else {
                    self.erase_cursor();
                }
            } else {
                self.draw_cursor();
            }
        } else {
            self.draw_status(now);
        }
        self.flush();
    }
}

/// Columns to indent wrapped continuation lines: under the text of
/// "1. ", "• ", "- ", "* ", "> ", "│ " markers (after any leading spaces).
pub fn hanging_indent(line: &str) -> usize {
    let lead = line.chars().take_while(|c| *c == ' ').count();
    let rest: String = line.chars().skip(lead).collect();
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let marker = if digits > 0 && rest[digits..].starts_with(". ") {
        digits + 2
    } else if ["• ", "- ", "* ", "> ", "│ ", "▌ ", "▍ ", "▏ "].iter().any(|m| rest.starts_with(m)) {
        2
    } else {
        0
    };
    let hang = lead + marker;
    if hang > 24 { 0 } else { hang }
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            skip_escape(&mut it);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_skips_ansi_and_counts_wide() {
        assert_eq!(visible_width("\x1b[1m\x1b[38;2;1;2;3mab\x1b[0m"), 2);
        assert_eq!(visible_width("📄 x"), 4);
    }

    #[test]
    fn clip_keeps_escapes_and_marks_cut() {
        let s = format!("{}hello world{}", tn::BLUE, tn::RESET);
        let c = clip_ansi(&s, 6);
        assert_eq!(strip_ansi(&c), "hello…");
        assert!(c.starts_with(tn::BLUE));
    }

    #[test]
    fn hanging_indents() {
        assert_eq!(hanging_indent("1. Run ls"), 3);
        assert_eq!(hanging_indent("12. x"), 4);
        assert_eq!(hanging_indent("• item"), 2);
        assert_eq!(hanging_indent("  • nested"), 4);
        assert_eq!(hanging_indent("plain text"), 0);
        assert_eq!(hanging_indent("3.14 is pi"), 0);
    }

    #[test]
    fn remap_headings_get_marks() {
        let h1 = "\x1b[1m\x1b[38;2;198;160;246mTitle\x1b[0m\n";
        assert_eq!(strip_ansi(&remap(h1)), "▌ Title\n");
        assert!(remap("plain").eq("plain"));
    }
}
