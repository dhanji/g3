//! Event streaming: emit structured NDJSON events for external UIs (e.g. butler.app).
//!
//! # Design
//!
//! `EventStreamWriter<W>` is a `UiWriter` decorator that wraps an inner writer
//! (typically `ConsoleUiWriter`), delegates every call to it, AND also emits a
//! JSON line to a sidecar file for the semantically-interesting events:
//!
//! - Token deltas (streaming assistant text, already filtered by g3's state machines)
//! - Tool call starts (name + args)
//! - Tool output lines (streaming or batch)
//! - Tool timing (duration, tokens delta, context %)
//! - Status/progress messages
//! - Turn end markers (finish_streaming_markdown)
//!
//! # Why NDJSON?
//!
//! One JSON object per line, flushed immediately. Consumers can tail the file
//! and parse line-by-line. Robust to concurrent readers, easy to jq.
//!
//! # Failure mode
//!
//! If writing fails (bad path, disk full, closed fd), we log ONCE to stderr,
//! disable further writes, and continue silently. Never panics, never blocks g3.

use g3_core::ui_writer::UiWriter;
use serde_json::json;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wraps a UiWriter and tees semantic events to a JSON-lines file.
pub struct EventStreamWriter<W: UiWriter> {
    inner: W,
    file: Mutex<Option<BufWriter<File>>>,
    disabled: AtomicBool,
    seq: AtomicU64,
    /// Track the currently-open tool call so we can attach output lines to it.
    /// (Reset on print_tool_timing / next print_tool_header.)
    current_tool: Mutex<Option<String>>,
}

impl<W: UiWriter> EventStreamWriter<W> {
    /// Create a new event stream writer wrapping `inner`, writing events to `path`.
    /// If `path` is None, events are silently disabled (the wrapper is a zero-cost
    /// pass-through). If the file cannot be opened, prints a warning to stderr
    /// and events are silently dropped.
    pub fn new(inner: W, path: Option<&Path>) -> Self {
        let file = match path {
            None => None,
            Some(p) => match File::create(p) {
                Ok(f) => Some(BufWriter::new(f)),
                Err(e) => {
                    eprintln!(
                        "warning: --stream-events could not open {}: {} (events disabled)",
                        p.display(),
                        e
                    );
                    None
                }
            },
        };
        // Disabled if no path provided OR if opening failed.
        let disabled = AtomicBool::new(file.is_none());
        Self {
            inner,
            file: Mutex::new(file),
            disabled,
            seq: AtomicU64::new(0),
            current_tool: Mutex::new(None),
        }
    }

    fn emit(&self, event_type: &str, payload: serde_json::Value) {
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut obj = serde_json::Map::new();
        obj.insert("seq".to_string(), json!(seq));
        obj.insert("ts".to_string(), json!(ts));
        obj.insert("type".to_string(), json!(event_type));
        if let serde_json::Value::Object(map) = payload {
            for (k, v) in map {
                obj.insert(k, v);
            }
        }
        let line = serde_json::Value::Object(obj).to_string();

        let mut guard = self.file.lock().unwrap();
        if let Some(ref mut f) = *guard {
            if let Err(e) = writeln!(f, "{}", line).and_then(|_| f.flush()) {
                eprintln!("warning: --stream-events write failed: {} (events disabled)", e);
                self.disabled.store(true, Ordering::Relaxed);
                *guard = None;
            }
        }
    }
}

impl<W: UiWriter> UiWriter for EventStreamWriter<W> {
    // ── Pass-through with no event emission ──────────────────────────────────
    fn print(&self, message: &str) {
        self.inner.print(message);
    }
    fn println(&self, message: &str) {
        self.inner.println(message);
    }
    fn print_inline(&self, message: &str) {
        self.inner.print_inline(message);
    }
    fn print_system_prompt(&self, prompt: &str) {
        self.inner.print_system_prompt(prompt);
    }
    fn print_agent_prompt(&self) {
        self.inner.print_agent_prompt();
    }

    // ── Status / progress ────────────────────────────────────────────────────
    fn print_context_status(&self, message: &str) {
        self.emit("context_status", json!({ "message": message }));
        self.inner.print_context_status(message);
    }
    fn print_g3_progress(&self, message: &str) {
        self.emit("g3_progress", json!({ "message": message }));
        self.inner.print_g3_progress(message);
    }
    fn print_g3_status(&self, message: &str, status: &str) {
        self.emit(
            "g3_status",
            json!({ "message": message, "status": status }),
        );
        self.inner.print_g3_status(message, status);
    }
    fn print_thin_result(&self, result: &g3_core::ThinResult) {
        self.emit(
            "thin_result",
            json!({
                "before_pct": result.before_percentage,
                "after_pct": result.after_percentage,
                "chars_saved": result.chars_saved,
                "leaned_count": result.leaned_count,
                "tool_call_leaned_count": result.tool_call_leaned_count,
            }),
        );
        self.inner.print_thin_result(result);
    }
    fn print_context_thinning(&self, message: &str) {
        self.emit("context_thinning", json!({ "message": message }));
        self.inner.print_context_thinning(message);
    }

    fn print_context_summary(&self, used_tokens: u32, total_tokens: u32, percentage: f32) {
        self.emit(
            "context_summary",
            json!({
                "used_tokens": used_tokens,
                "total_tokens": total_tokens,
                "pct": percentage,
            }),
        );
        self.inner.print_context_summary(used_tokens, total_tokens, percentage);
    }

    // ── Streaming assistant text ─────────────────────────────────────────────
    fn print_agent_response(&self, content: &str) {
        // content has already been through clean_llm_tokens + filter_json_tool_calls
        // by the time it reaches print_agent_response. We tee it as-is.
        if !content.is_empty() {
            self.emit("token_delta", json!({ "text": content }));
        }
        self.inner.print_agent_response(content);
    }

    fn finish_streaming_markdown(&self) {
        self.emit("assistant_message_end", json!({}));
        self.inner.finish_streaming_markdown();
    }

    // ── Tool calls ───────────────────────────────────────────────────────────
    fn print_tool_header(&self, tool_name: &str, tool_args: Option<&serde_json::Value>) {
        *self.current_tool.lock().unwrap() = Some(tool_name.to_string());
        self.emit(
            "tool_call",
            json!({
                "name": tool_name,
                "args": tool_args.cloned().unwrap_or(serde_json::Value::Null),
            }),
        );
        self.inner.print_tool_header(tool_name, tool_args);
    }
    fn print_tool_arg(&self, key: &str, value: &str) {
        self.emit("tool_arg", json!({ "key": key, "value": value }));
        self.inner.print_tool_arg(key, value);
    }
    fn print_tool_output_header(&self) {
        self.emit("tool_output_start", json!({}));
        self.inner.print_tool_output_header();
    }
    fn update_tool_output_line(&self, line: &str) {
        self.emit(
            "tool_output_line",
            json!({
                "tool": self.current_tool.lock().unwrap().clone(),
                "text": line,
                "streaming": true,
            }),
        );
        self.inner.update_tool_output_line(line);
    }
    fn print_tool_output_line(&self, line: &str) {
        self.emit(
            "tool_output_line",
            json!({
                "tool": self.current_tool.lock().unwrap().clone(),
                "text": line,
                "streaming": false,
            }),
        );
        self.inner.print_tool_output_line(line);
    }
    fn print_tool_output_summary(&self, hidden_count: usize) {
        self.emit(
            "tool_output_summary",
            json!({ "hidden": hidden_count }),
        );
        self.inner.print_tool_output_summary(hidden_count);
    }
    fn print_tool_compact(
        &self,
        tool_name: &str,
        summary: &str,
        duration_str: &str,
        tokens_delta: u32,
        context_percentage: f32,
    ) -> bool {
        self.emit(
            "tool_compact",
            json!({
                "name": tool_name,
                "summary": summary,
                "duration": duration_str,
                "tokens_delta": tokens_delta,
                "context_pct": context_percentage,
            }),
        );
        self.inner
            .print_tool_compact(tool_name, summary, duration_str, tokens_delta, context_percentage)
    }
    fn print_todo_compact(&self, content: Option<&str>, is_write: bool) -> bool {
        self.emit(
            "tool_todo",
            json!({ "content": content, "is_write": is_write }),
        );
        self.inner.print_todo_compact(content, is_write)
    }
    fn print_plan_compact(
        &self,
        plan_yaml: Option<&str>,
        plan_file_path: Option<&str>,
        is_write: bool,
    ) -> bool {
        self.emit(
            "tool_plan",
            json!({
                "plan_yaml": plan_yaml,
                "plan_file_path": plan_file_path,
                "is_write": is_write,
            }),
        );
        self.inner.print_plan_compact(plan_yaml, plan_file_path, is_write)
    }
    fn print_envelope_compact(
        &self,
        fact_groups: usize,
        stages: &[(&str, &str)],
        passed: Option<usize>,
        total: Option<usize>,
        failed: usize,
    ) {
        let stages_json: Vec<serde_json::Value> = stages
            .iter()
            .map(|(icon, desc)| json!({ "icon": icon, "desc": desc }))
            .collect();
        self.emit(
            "tool_envelope",
            json!({
                "fact_groups": fact_groups,
                "stages": stages_json,
                "passed": passed,
                "total": total,
                "failed": failed,
            }),
        );
        self.inner
            .print_envelope_compact(fact_groups, stages, passed, total, failed);
    }
    fn print_tool_timing(&self, duration_str: &str, tokens_delta: u32, context_percentage: f32) {
        let tool = self.current_tool.lock().unwrap().take();
        self.emit(
            "tool_end",
            json!({
                "tool": tool,
                "duration": duration_str,
                "tokens_delta": tokens_delta,
                "context_pct": context_percentage,
            }),
        );
        self.inner
            .print_tool_timing(duration_str, tokens_delta, context_percentage);
    }

    // ── Streaming hints ──────────────────────────────────────────────────────
    fn notify_sse_received(&self) {
        self.inner.notify_sse_received();
    }
    fn print_tool_streaming_hint(&self, tool_name: &str) {
        self.emit("tool_streaming_hint", json!({ "name": tool_name }));
        self.inner.print_tool_streaming_hint(tool_name);
    }
    fn print_tool_streaming_active(&self) {
        // Skip emitting these — they are just blink-indicator ticks, not
        // semantic events. Keeps the NDJSON stream clean.
        self.inner.print_tool_streaming_active();
    }

    // ── Misc pass-through ────────────────────────────────────────────────────
    fn flush(&self) {
        self.inner.flush();
    }
    fn wants_full_output(&self) -> bool {
        self.inner.wants_full_output()
    }
    fn prompt_user_yes_no(&self, message: &str) -> bool {
        // Note: interactive prompts don't happen in agent mode used by butler.app,
        // but we delegate faithfully in case they do.
        self.inner.prompt_user_yes_no(message)
    }
    fn prompt_user_choice(&self, message: &str, options: &[&str]) -> usize {
        self.inner.prompt_user_choice(message, options)
    }
    fn filter_json_tool_calls(&self, content: &str) -> String {
        // Delegate — the filtering is the inner writer's job (its state machine).
        self.inner.filter_json_tool_calls(content)
    }
    fn reset_json_filter(&self) {
        self.inner.reset_json_filter();
    }
    fn set_agent_mode(&self, is_agent_mode: bool) {
        self.inner.set_agent_mode(is_agent_mode);
    }
    fn set_workspace_path(&self, path: std::path::PathBuf) {
        self.inner.set_workspace_path(path);
    }
    fn set_project_path(&self, path: std::path::PathBuf, name: String) {
        self.inner.set_project_path(path, name);
    }
    fn clear_project(&self) {
        self.inner.clear_project();
    }
}
