//! Which UI writer this process uses — decided ONCE, at startup.
//!
//! `Classic` delegates every `UiWriter` method, including the defaulted ones,
//! 1:1 to the untouched `ConsoleUiWriter`, so anything that isn't a human at a
//! terminal (butler.app, duties, studio, scout) sees byte-identical output.
//! A test scans `g3-core/src/ui_writer.rs` and fails if a trait method is ever
//! added without being delegated here — a missed default would otherwise
//! silently change classic behaviour.

use crate::pretty::{self, PrettyUiWriter};
use crate::ui_writer_impl::ConsoleUiWriter;
use g3_core::ui_writer::UiWriter;
use std::io::IsTerminal;

pub enum CliUiWriter {
    Classic(ConsoleUiWriter),
    Pretty(PrettyUiWriter),
}

impl CliUiWriter {
    /// Pick the writer from the real environment.
    pub fn from_env(quiet: bool) -> Self {
        let tty = std::io::stdout().is_terminal();
        let env = std::env::var("G3_UI").ok();
        if pretty::choose(tty, quiet, env.as_deref()) {
            CliUiWriter::Pretty(PrettyUiWriter::new())
        } else {
            CliUiWriter::Classic(ConsoleUiWriter::new())
        }
    }

    pub fn is_pretty(&self) -> bool {
        matches!(self, CliUiWriter::Pretty(_))
    }
}

macro_rules! delegate {
    ($self:ident . $m:ident ( $($a:expr),* )) => {
        match $self {
            CliUiWriter::Classic(w) => w.$m($($a),*),
            CliUiWriter::Pretty(w) => w.$m($($a),*),
        }
    };
}

impl UiWriter for CliUiWriter {
    fn print(&self, message: &str) { delegate!(self.print(message)) }
    fn println(&self, message: &str) { delegate!(self.println(message)) }
    fn print_inline(&self, message: &str) { delegate!(self.print_inline(message)) }
    fn print_system_prompt(&self, prompt: &str) { delegate!(self.print_system_prompt(prompt)) }
    fn print_context_status(&self, message: &str) { delegate!(self.print_context_status(message)) }
    fn print_g3_progress(&self, message: &str) { delegate!(self.print_g3_progress(message)) }
    fn print_g3_status(&self, message: &str, status: &str) { delegate!(self.print_g3_status(message, status)) }
    fn print_thin_result(&self, result: &g3_core::ThinResult) { delegate!(self.print_thin_result(result)) }
    fn print_context_thinning(&self, message: &str) { delegate!(self.print_context_thinning(message)) }
    fn print_context_summary(&self, used_tokens: u32, total_tokens: u32, percentage: f32) {
        delegate!(self.print_context_summary(used_tokens, total_tokens, percentage))
    }
    fn print_user_injected(&self, text: &str) { delegate!(self.print_user_injected(text)) }
    fn print_tool_header(&self, tool_name: &str, tool_args: Option<&serde_json::Value>) {
        delegate!(self.print_tool_header(tool_name, tool_args))
    }
    fn print_tool_arg(&self, key: &str, value: &str) { delegate!(self.print_tool_arg(key, value)) }
    fn print_tool_output_header(&self) { delegate!(self.print_tool_output_header()) }
    fn update_tool_output_line(&self, line: &str) { delegate!(self.update_tool_output_line(line)) }
    fn print_tool_output_line(&self, line: &str) { delegate!(self.print_tool_output_line(line)) }
    fn print_tool_output_summary(&self, hidden_count: usize) { delegate!(self.print_tool_output_summary(hidden_count)) }
    fn print_tool_compact(&self, tool_name: &str, summary: &str, duration_str: &str, tokens_delta: u32, context_percentage: f32) -> bool {
        delegate!(self.print_tool_compact(tool_name, summary, duration_str, tokens_delta, context_percentage))
    }
    fn print_todo_compact(&self, content: Option<&str>, is_write: bool) -> bool {
        delegate!(self.print_todo_compact(content, is_write))
    }
    fn print_plan_compact(&self, plan_yaml: Option<&str>, plan_file_path: Option<&str>, is_write: bool) -> bool {
        delegate!(self.print_plan_compact(plan_yaml, plan_file_path, is_write))
    }
    fn print_envelope_compact(&self, fact_groups: usize, stages: &[(&str, &str)], passed: Option<usize>, total: Option<usize>, failed: usize) {
        delegate!(self.print_envelope_compact(fact_groups, stages, passed, total, failed))
    }
    fn print_tool_timing(&self, duration_str: &str, tokens_delta: u32, context_percentage: f32) {
        delegate!(self.print_tool_timing(duration_str, tokens_delta, context_percentage))
    }
    fn print_agent_prompt(&self) { delegate!(self.print_agent_prompt()) }
    fn print_agent_response(&self, content: &str) { delegate!(self.print_agent_response(content)) }
    fn notify_sse_received(&self) { delegate!(self.notify_sse_received()) }
    fn notify_upstream_ping(&self) { delegate!(self.notify_upstream_ping()) }
    fn notify_tool_heartbeat(&self, elapsed_secs: u64) { delegate!(self.notify_tool_heartbeat(elapsed_secs)) }
    fn print_tool_streaming_hint(&self, tool_name: &str) { delegate!(self.print_tool_streaming_hint(tool_name)) }
    fn print_tool_streaming_active(&self) { delegate!(self.print_tool_streaming_active()) }
    fn flush(&self) { delegate!(self.flush()) }
    fn wants_full_output(&self) -> bool { delegate!(self.wants_full_output()) }
    fn prompt_user_yes_no(&self, message: &str) -> bool { delegate!(self.prompt_user_yes_no(message)) }
    fn prompt_user_choice(&self, message: &str, options: &[&str]) -> usize { delegate!(self.prompt_user_choice(message, options)) }
    fn filter_json_tool_calls(&self, content: &str) -> String { delegate!(self.filter_json_tool_calls(content)) }
    fn reset_json_filter(&self) { delegate!(self.reset_json_filter()) }
    fn finish_streaming_markdown(&self) { delegate!(self.finish_streaming_markdown()) }
    fn set_agent_mode(&self, is_agent_mode: bool) { delegate!(self.set_agent_mode(is_agent_mode)) }
    fn set_workspace_path(&self, path: std::path::PathBuf) { delegate!(self.set_workspace_path(path)) }
    fn set_project_path(&self, path: std::path::PathBuf, name: String) { delegate!(self.set_project_path(path, name)) }
    fn clear_project(&self) { delegate!(self.clear_project()) }
}

#[cfg(test)]
mod tests {
    /// Every method declared on the UiWriter trait must be delegated above.
    /// A defaulted method that is NOT delegated would make Classic call the
    /// trait default instead of ConsoleUiWriter's override — a silent change
    /// to the output butler.app and every duty depend on.
    #[test]
    fn every_trait_method_is_delegated() {
        let trait_src = include_str!("../../g3-core/src/ui_writer.rs");
        let here = include_str!("ui_select.rs");
        let body = trait_src.split("pub trait UiWriter").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let mut names = Vec::new();
        for line in body.lines() {
            let l = line.trim_start();
            if let Some(rest) = l.strip_prefix("fn ") {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                names.push(name);
            }
        }
        assert!(names.len() >= 35, "parsed too few trait methods ({}) — parser broke?", names.len());
        let impl_part = here.split("impl UiWriter for CliUiWriter").nth(1).unwrap();
        let mut missing = Vec::new();
        for n in &names {
            let needle = format!("delegate!(self.{}(", n);
            if !impl_part.contains(&needle) {
                missing.push(n.clone());
            }
        }
        assert!(missing.is_empty(), "UiWriter methods not delegated by CliUiWriter: {:?}", missing);
    }
}
