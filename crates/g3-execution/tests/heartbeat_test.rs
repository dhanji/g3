//! Tool-execution heartbeat: proving a silent command still reports life.
//!
//! # Why this exists
//!
//! butler.app kills a wedged g3 by watching its NDJSON events file stop
//! growing. Anthropic's SSE keep-alive covers a long THINK, but it stops dead
//! during tool execution — there is no request in flight while a subprocess
//! runs. Measured in the local corpus: 434 pings arrived between tool calls and
//! only 50 during one, with 43% of all wall time inside tool execution.
//!
//! So a silent `cargo test` was byte-identical to a hung process, and the idle
//! budget had to be set above the longest tool that could ever run — coupling
//! butler.app's constant to g3's per-tool timeout (8 min, 20 for `research`).
//! The heartbeat decouples them.
//!
//! # What must hold
//!
//! 1. a long SILENT command still produces beats (the whole point)
//! 2. a fast command produces none (no noise on the common path)
//! 3. beats STOP when the command does (a beat after exit is a lie)
//! 4. a CHATTY command still delivers every line, in order, undamaged
//!
//! (4) is the one that looks unnecessary and is not: the beat was added as a
//! branch of the same `tokio::select!` that reads stdout, so a mistake there
//! drops or reorders real output. `select!` picks a READY branch at random,
//! which is exactly the shape of bug that shows up once in fifty runs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use g3_execution::{CodeExecutor, OutputReceiver};

/// Records everything a receiver is told, so tests can assert on ORDER as well
/// as on counts.
#[derive(Default)]
struct Recorder {
    lines: Mutex<Vec<String>>,
    beats: AtomicU64,
    /// Beats seen after the last line — the "did it stop?" probe.
    beats_at_last_line: AtomicU64,
}

impl OutputReceiver for Recorder {
    fn on_output_line(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_string());
        self.beats_at_last_line
            .store(self.beats.load(Ordering::SeqCst), Ordering::SeqCst);
    }
    fn on_heartbeat(&self, _elapsed_secs: u64) {
        self.beats.fetch_add(1, Ordering::SeqCst);
    }
}

fn fast_beat() -> CodeExecutor {
    // 50ms rather than the real 30s: a test proving the production cadence
    // would take a minute to observe a single beat.
    CodeExecutor::with_heartbeat_interval(Duration::from_millis(50))
}

// ── happy path ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_silent_command_still_reports_life() {
    // THE REGRESSION TEST. `sleep` writes nothing at all — before the heartbeat
    // this stretch was indistinguishable from a wedged process, and it is the
    // exact shape of the 480s `cargo test` that survived only because
    // butler.app's idle budget had been inflated to accommodate it.
    let rec = Recorder::default();
    fast_beat()
        .execute_bash_streaming("sleep 0.6", &rec)
        .await
        .expect("command failed");

    let beats = rec.beats.load(Ordering::SeqCst);
    assert!(
        beats >= 5,
        "a 600ms silent command at a 50ms cadence should beat ~11 times, got {beats} \
         — a silent tool is still invisible to butler.app's idle guard",
    );
    assert!(
        rec.lines.lock().unwrap().is_empty(),
        "sleep produced output?",
    );
}

// ── negative ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_fast_command_does_not_beat() {
    // Noise control: the common case is a command finishing in milliseconds,
    // and a beat for every one of those would bloat the events file for no
    // information. The first `interval` tick fires immediately, so this fails
    // if that tick is not discarded.
    let rec = Recorder::default();
    fast_beat()
        .execute_bash_streaming("echo quick", &rec)
        .await
        .expect("command failed");

    assert_eq!(
        rec.beats.load(Ordering::SeqCst),
        0,
        "a command that finished instantly emitted a heartbeat — the immediate \
         first tick is not being discarded",
    );
}

#[tokio::test]
async fn beats_stop_when_the_command_does() {
    // A beat after exit would be a lie about a process that is gone, and would
    // keep butler.app's idle guard armed for a turn that had already finished.
    let rec = Recorder::default();
    fast_beat()
        .execute_bash_streaming("sleep 0.3", &rec)
        .await
        .expect("command failed");

    let at_exit = rec.beats.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rec.beats.load(Ordering::SeqCst),
        at_exit,
        "the heartbeat kept firing after the command exited",
    );
}

// ── boundary ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_chatty_command_loses_no_output_to_the_beat() {
    // ⚠️ THE BRANCH-INTERFERENCE CASE. The beat shares a `select!` with the two
    // stdout/stderr readers, and `select!` chooses among READY branches at
    // random — so a timer branch that returned early, or one placed where it
    // could consume a line, drops output nondeterministically.
    //
    // 40 lines with a beat every 50ms guarantees the branches genuinely race.
    let rec = Recorder::default();
    fast_beat()
        .execute_bash_streaming(
            "for i in $(seq 1 40); do echo line$i; sleep 0.01; done",
            &rec,
        )
        .await
        .expect("command failed");

    let lines = rec.lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 40, "lost output lines to the heartbeat branch");
    let expected: Vec<String> = (1..=40).map(|i| format!("line{i}")).collect();
    assert_eq!(lines, expected, "output was reordered by the heartbeat branch");
}

#[tokio::test]
async fn output_resets_nothing_but_the_command_still_beats_when_it_pauses() {
    // A tool that prints, then goes quiet for a long time (a build that logs a
    // header then compiles in silence) — the realistic shape of the 8-minute
    // timeouts in the corpus. Output must arrive AND the silent tail must beat.
    let rec = Recorder::default();
    fast_beat()
        .execute_bash_streaming("echo starting; sleep 0.5; echo done", &rec)
        .await
        .expect("command failed");

    let lines = rec.lines.lock().unwrap().clone();
    assert_eq!(lines, vec!["starting", "done"], "output damaged: {lines:?}");

    let total = rec.beats.load(Ordering::SeqCst);
    let before_last_line = rec.beats_at_last_line.load(Ordering::SeqCst);
    assert!(
        total >= 4,
        "the silent middle stretch should have beaten several times, got {total}",
    );
    assert!(
        before_last_line >= 4,
        "beats should accumulate DURING the pause, before the final line \
         (saw {before_last_line} by then, {total} total)",
    );
}

#[tokio::test]
async fn a_failing_command_still_beat_while_it_ran() {
    // Liveness is not success. A long-running command that ultimately exits
    // non-zero was still alive throughout, and butler.app must not have been
    // free to kill it.
    let rec = Recorder::default();
    let result = fast_beat()
        .execute_bash_streaming("sleep 0.4; exit 7", &rec)
        .await
        .expect("execution itself should not error");

    assert_eq!(result.exit_code, 7, "exit code not propagated");
    assert!(!result.success);
    assert!(
        rec.beats.load(Ordering::SeqCst) >= 3,
        "a command that ran for 400ms before failing produced too few beats",
    );
}
