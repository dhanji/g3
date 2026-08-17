//! Upstream keep-alive (`ping`) plumbing.
//!
//! WHAT THIS IS FOR
//! ----------------
//! Between a tool finishing and the model's first token there is a silence, and
//! butler.app terminates a turn that stays silent for POST_TOOL_STALL_SEC (90s).
//! Re-measured 2026-08-17 over 439 real post-tool gaps in butler's event corpus:
//!
//!     p50 2.7s   p75 4.6s   p90 10.0s   p95 21.1s   p99 85.5s   max 141.3s
//!
//! Eleven gaps exceeded 60s and three exceeded 90s. The worst — 141.3s — was a
//! perfectly healthy plan-mode turn that the 90s fuse would have killed, and
//! butler would have shown a dead spinner for it.
//!
//! Nothing butler could observe distinguished that from a wedge:
//!
//!     signal            healthy   wedged (SIGSTOP)   dead    discriminates?
//!     pid_alive         true      true               false   NO
//!     events file size  +bytes    0                  0       only if writing
//!
//! and during a long think g3 writes nothing at all — zero records landed inside
//! every one of those 11 gaps. So byte growth could not rescue them either.
//!
//! Anthropic, however, sends an SSE `ping` every 30.0s while the model thinks
//! (measured: three consecutive intervals of exactly 30.00s across a 129.5s
//! opus-5 request whose first text arrived at 85.4s). That frame is the ONLY
//! liveness evidence available that does not originate inside g3, and it used to
//! be dropped by the `_ =>` catch-all in anthropic.rs — which is why the
//! "including pings" comment on `notify_sse_received` was false.
//!
//! These tests pin the plumbing: a ping must become an observable signal, and it
//! must NOT be mistakable for model output.

use g3_core::ui_writer::UiWriter;
use g3_core::Agent;
use g3_providers::mock::{MockChunk, MockProvider, MockResponse};
use g3_providers::{ProviderRegistry, Usage};

fn zero_usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}
use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

/// Counts the two notifications separately.
///
/// `notify_sse_received` already fired for every chunk, so counting only that
/// would pass whether or not the ping was surfaced — the mistake this counter
/// shape exists to avoid (skills/testing: "a presence counter is worthless when
/// two paths call the same function").
#[derive(Clone)]
struct PingCountingWriter {
    pings: Arc<AtomicUsize>,
    sse: Arc<AtomicUsize>,
    text: Arc<std::sync::Mutex<String>>,
}

impl PingCountingWriter {
    fn new() -> Self {
        Self {
            pings: Arc::new(AtomicUsize::new(0)),
            sse: Arc::new(AtomicUsize::new(0)),
            text: Arc::new(std::sync::Mutex::new(String::new())),
        }
    }
    fn pings(&self) -> usize {
        self.pings.load(Ordering::SeqCst)
    }
    fn sse(&self) -> usize {
        self.sse.load(Ordering::SeqCst)
    }
    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }
}

impl UiWriter for PingCountingWriter {
    fn print(&self, _m: &str) {}
    fn println(&self, _m: &str) {}
    fn print_inline(&self, _m: &str) {}
    fn print_system_prompt(&self, _p: &str) {}
    fn print_context_status(&self, _m: &str) {}
    fn print_g3_progress(&self, _m: &str) {}
    fn print_g3_status(&self, _m: &str, _s: &str) {}
    fn print_thin_result(&self, _r: &g3_core::ThinResult) {}
    fn print_tool_header(&self, _n: &str, _a: Option<&serde_json::Value>) {}
    fn print_tool_arg(&self, _k: &str, _v: &str) {}
    fn print_tool_output_header(&self) {}
    fn update_tool_output_line(&self, _l: &str) {}
    fn print_tool_output_line(&self, _l: &str) {}
    fn print_tool_output_summary(&self, _hidden: usize) {}
    fn print_tool_timing(&self, _d: &str, _t: u32, _c: f32) {}
    fn print_agent_prompt(&self) {}
    fn print_agent_response(&self, content: &str) {
        self.text.lock().unwrap().push_str(content);
    }
    fn notify_sse_received(&self) {
        self.sse.fetch_add(1, Ordering::SeqCst);
    }
    fn notify_upstream_ping(&self) {
        self.pings.fetch_add(1, Ordering::SeqCst);
    }
    fn print_tool_streaming_hint(&self, _t: &str) {}
    fn print_tool_streaming_active(&self) {}
    fn flush(&self) {}
    fn prompt_user_yes_no(&self, _m: &str) -> bool {
        false
    }
    fn prompt_user_choice(&self, _m: &str, _o: &[&str]) -> usize {
        0
    }
}

async fn agent_with(provider: MockProvider, w: PingCountingWriter)
    -> (Agent<PingCountingWriter>, TempDir)
{
    let temp = TempDir::new().unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(provider);
    let agent = Agent::new_for_test(g3_config::Config::default(), w, registry)
        .await
        .expect("agent");
    (agent, temp)
}

/// Happy path: a `ping` chunk reaches the UI writer as its own signal.
#[tokio::test]
#[serial]
async fn a_ping_chunk_notifies_the_ui_writer() {
    let w = PingCountingWriter::new();
    let provider = MockProvider::new().with_response(MockResponse::custom(
        vec![
            MockChunk::upstream_ping(),
            MockChunk::content("done thinking"),
            MockChunk::finished("end_turn"),
        ],
        zero_usage(),
    ));
    let (mut agent, _t) = agent_with(provider, w.clone()).await;
    let _ = agent.execute_task("hi", None, false).await;

    assert_eq!(
        w.pings(),
        1,
        "the ping never reached the UI writer, so butler.app can never learn the \
         upstream was alive during a long think"
    );
    // The two notifications must be DISTINCT. notify_sse_received() already fired
    // for every chunk before this change, so a ping signal that merely reused it
    // would be indistinguishable from the status quo — and butler.app would have
    // no way to tell "a chunk arrived" from "the upstream is alive".
    assert!(
        w.sse() > w.pings(),
        "notify_sse_received fired {} times vs {} pings — the ping is not a \
         separate signal from the generic per-chunk notification",
        w.sse(),
        w.pings()
    );
}

/// Negative: a ping must not be mistaken for model output.
///
/// This is the failure mode that would be WORSE than the bug — a keep-alive
/// counted as content would suppress the empty-stream retry (which keys off
/// `has_text_response`) and let a genuinely empty stream look answered.
#[tokio::test]
#[serial]
async fn a_ping_contributes_no_text_to_the_response() {
    let w = PingCountingWriter::new();
    let provider = MockProvider::new().with_response(MockResponse::custom(
        vec![
            MockChunk::upstream_ping(),
            MockChunk::upstream_ping(),
            MockChunk::content("real answer"),
            MockChunk::finished("end_turn"),
        ],
        zero_usage(),
    ));
    let (mut agent, _t) = agent_with(provider, w.clone()).await;
    agent.execute_task("hi", None, false).await.expect("task");

    assert_eq!(w.pings(), 2, "expected both pings to be surfaced");
    // Assert on the CONVERSATION HISTORY, which is what the rest of this suite
    // treats as the observable (TaskResult.response is empty for a text-only
    // mock response — verified with a no-ping control, so blaming the ping path
    // for it would have been wrong).
    let history = &agent.get_context_window().conversation_history;
    let last = history.last().expect("a reply was recorded");
    assert!(
        last.content.contains("real answer"),
        "the real content was lost around the pings: {:?}",
        last.content
    );
    // A ping chunk is empty by construction, so it can contribute no characters.
    // Asserting on the accumulated stream text (not just the return value) is
    // what catches a ping that leaked in as whitespace or an empty token.
    assert!(
        !w.text().contains("ping"),
        "ping leaked into the streamed text: {:?}",
        w.text()
    );
}

/// Negative: an unknown future event type must stay ignored.
///
/// The ping branch was added ALONGSIDE the `_ =>` catch-all, not in place of it.
/// If someone later turns the catch-all into a ping-producing default, every
/// unrecognised Anthropic frame starts asserting liveness — which would make the
/// signal untrustworthy in exactly the way pid_alive already is.
#[test]
fn only_the_ping_frame_type_produces_a_ping() {
    let src = include_str!("../../g3-providers/src/anthropic.rs");
    assert!(
        src.contains(r#""ping" => vec![Ok(make_upstream_ping_chunk())]"#),
        "the ping arm is missing or reshaped; pings are being discarded again"
    );
    // The catch-all must still be a plain ignore.
    assert!(
        src.contains(r#"_ => { debug!("Ignoring event type: {}", event.event_type); vec![] }"#),
        "the catch-all no longer plainly ignores unknown frames — an unrecognised \
         event type may now be asserting upstream liveness"
    );
}

/// Boundary: a ping arriving after `message_stop` is discarded, as before.
///
/// `state.message_stopped` short-circuits the whole line loop before the event is
/// even parsed, so this is a property of code that PRECEDES the new arm. Pinned
/// because the ping arm would otherwise be a plausible place to "helpfully"
/// bypass it, and a keep-alive after the message ends is precisely the shape that
/// would keep a finished turn spinning.
#[test]
fn the_post_message_stop_guard_precedes_event_dispatch() {
    let src = include_str!("../../g3-providers/src/anthropic.rs");
    let guard = src
        .find("if line.is_empty() || state.message_stopped {")
        .expect("the message_stopped short-circuit is gone");
    let dispatch = src
        .find(r#""ping" => vec![Ok(make_upstream_ping_chunk())]"#)
        .expect("the ping arm is gone");
    assert!(
        guard < dispatch,
        "the message_stopped guard no longer precedes dispatch, so a ping after \
         message_stop could now be forwarded"
    );
}

/// CONTROL — no pings at all. If this is also empty, the emptiness is a property
/// of the harness (MockResponse::custom / execute_task), not of the ping path,
/// and the sibling assertion would be blaming the wrong thing.
#[tokio::test]
#[serial]
async fn control_no_pings_still_returns_content() {
    let w = PingCountingWriter::new();
    let provider = MockProvider::new().with_response(MockResponse::text("real answer"));
    let (mut agent, _t) = agent_with(provider, w.clone()).await;
    agent.execute_task("hi", None, false).await.expect("task");
    let history = &agent.get_context_window().conversation_history;
    let last = history.last().expect("a reply was recorded");
    assert!(last.content.contains("real answer"),
        "CONTROL FAILED — the harness loses content even with NO pings, so the \
         sibling assertion would be blaming the ping path for a harness property: {:?}",
        last.content);
    assert_eq!(w.pings(), 0, "no pings were fed, yet some were reported");
}
