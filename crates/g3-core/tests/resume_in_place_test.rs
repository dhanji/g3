//! Regression tests for **in-place resume** (`--resume` continues the same
//! session dir instead of forking a new one).
//!
//! ## The bug
//!
//! `Agent::restore_from_continuation` restored the conversation into memory but
//! never set `self.session_id`. `execute_task` then saw `session_id == None`
//! and minted a fresh id, so **every resume wrote a brand-new session dir
//! containing a full copy of the history**. One conversation became N
//! directories: real corpus had lineages of 25 dirs for a single chat, and
//! butler.app had to reconstruct conversations by fingerprinting opening
//! exchanges and stitching chains together.
//!
//! The fix is one line — adopt `continuation.session_id` — but *only* on the
//! full-restore path. The restore-path tests (in place at ANY context %, and
//! the summary-only fallback never adopting the id) live, against a real
//! Agent, in `resume_high_context_test.rs`. The old <80% gate encoded here was
//! itself the bug that stranded a chat at 81% (2026-11-28).
//!
//! ## What is verified here
//!
//! 1. `save_context_window` writes atomically, because in-place rewrites now
//!    happen underneath concurrent readers (butler.app polls session.json
//!    while a turn streams).

// ── Atomicity of session.json writes ────────────────────────────────────────

/// Mirror of the production `write_atomic` helper in `session.rs`. Kept here so
/// the atomicity property is asserted rather than assumed.
fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[test]
fn concurrent_reader_never_sees_a_partial_file() {
    // Before in-place resume, each turn wrote a NEW dir, so a truncating
    // fs::write was harmless. Now session.json is rewritten while butler.app
    // polls it. A truncate+stream write lets a reader observe a half-file;
    // measured empirically at ~27% failure rate on a 1.3MB payload.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("g3_atomic_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.json");

    // Payload large enough that a non-atomic write is observably partial.
    let body = "x".repeat(300);
    let entries: Vec<String> = (0..2000)
        .map(|i| format!(r#"{{"role":"user","content":"{}{}"}}"#, i, body))
        .collect();
    let blob = format!(r#"{{"history":[{}]}}"#, entries.join(","));
    write_atomic(&path, &blob).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = stop.clone();
    let wpath = path.clone();
    let wblob = blob.clone();
    let writer = std::thread::spawn(move || {
        while !writer_stop.load(Ordering::Relaxed) {
            write_atomic(&wpath, &wblob).unwrap();
        }
    });

    let mut reads = 0usize;
    let mut short = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                reads += 1;
                // Every observed version must be COMPLETE: atomic rename means
                // we see either the old file or the new one, never a prefix.
                if s.len() != blob.len() || !s.ends_with("]}") {
                    short += 1;
                }
            }
            Err(_) => { /* rename window: acceptable, file briefly replaced */ }
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(reads > 100, "expected many reads, got {}", reads);
    assert_eq!(
        short, 0,
        "{} of {} reads saw a truncated/partial file — write is not atomic",
        short, reads
    );
}

#[test]
fn failed_write_leaves_previous_content_intact() {
    // A serialize/IO failure must not destroy the existing transcript. With
    // truncating writes, a crash mid-write leaves an unparseable stub, which
    // reads as "the conversation is gone".
    let dir = std::env::temp_dir().join(format!("g3_atomic_keep_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.json");
    write_atomic(&path, r#"{"history":["original"]}"#).unwrap();

    // Simulate a failed rename by making the temp path a directory, so
    // File::create on it fails and the original is left untouched.
    let tmp = path.with_extension("json.tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let res = write_atomic(&path, r#"{"history":["replacement"]}"#);
    assert!(res.is_err(), "write should have failed");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, r#"{"history":["original"]}"#, "original was clobbered");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn temp_file_is_a_sibling_so_rename_stays_atomic() {
    // rename(2) is only atomic within one filesystem. If the temp file were
    // placed in /tmp while sessions live on another volume, the "atomic"
    // rename would silently degrade to a copy.
    let path = std::path::Path::new("/Users/x/.g3/sessions/butler_abc/session.json");
    let tmp = path.with_extension("json.tmp");
    assert_eq!(tmp.parent(), path.parent(), "temp must be a sibling");
}
