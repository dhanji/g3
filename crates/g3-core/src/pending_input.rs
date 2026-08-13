//! Mid-turn user input mailbox.
//!
//! While a turn is in flight, an external process (butler.app, or anything else
//! holding the session id) can queue additional user messages for the running
//! agent. Each message is a separate file in
//! `.g3/sessions/<session_id>/inbox/`; the agent drains the directory at the
//! top of every streaming iteration and injects what it finds as user turns.
//!
//! # Why the filesystem
//!
//! The producer is a *different process* — butler.app spawns `g3` as a
//! subprocess with `stdin` set to DEVNULL — so this cannot be an in-process
//! channel like [`crate::pending_research`] uses. Every other g3/butler.app
//! interop point is already the session directory (`session.json`, `plan.g3.md`,
//! `envelope.yaml`, `--stream-events`), so the mailbox follows suit rather than
//! introducing a socket that would need its own lifecycle and cleanup.
//!
//! # Why one file per message
//!
//! A single shared JSON file would require read-modify-write from the producer,
//! and two concurrent sends would clobber one another (the same class of bug
//! that made g3 write `session.json` atomically). One file per message makes
//! each write independent: `create_new` cannot silently overwrite, and drain
//! order is filename order.
//!
//! # Delivery semantics
//!
//! At-most-once, and deliberately so: a message is deleted as it is read. If the
//! turn dies immediately after the read, that message is lost rather than
//! replayed into an unrelated future turn. Replaying is the worse failure — a
//! stale interjection arriving hours later, with no context, reads as the agent
//! hallucinating a user request.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Maximum size of a single queued message, in bytes.
///
/// Guards against a runaway producer wedging a turn with a multi-megabyte
/// paste. Anything larger is truncated on read, not rejected — a truncated
/// interjection is still useful, a dropped one is silently confusing.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Filename extension for queued messages. Files not matching are ignored, so
/// a producer can write to a temp name and rename into place atomically.
const MESSAGE_EXT: &str = "msg";

/// A message that was queued while a turn was running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    /// The raw user text.
    pub text: String,
    /// The file the message was read from, for logging.
    pub source: PathBuf,
}

/// Generate a filename that sorts chronologically.
///
/// Format: `<millis>_<counter>.msg`. The counter disambiguates two messages
/// queued inside the same millisecond, so ordering is total rather than
/// merely probable. Zero-padded because this is sorted as a *string*.
pub fn generate_filename() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:013}_{:06}.{}", millis, counter, MESSAGE_EXT)
}

/// Queue a message for a running turn.
///
/// Writes to a temporary dotfile and renames into place, so a drain running
/// concurrently never observes a partially-written message. Used by tests and
/// by any Rust-side producer; butler.app does the equivalent in Python.
pub fn enqueue(inbox_dir: &Path, text: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(inbox_dir)?;
    let name = generate_filename();
    // Leading dot: skipped by the drain's extension filter even mid-write.
    let tmp = inbox_dir.join(format!(".{}.partial", name));
    let final_path = inbox_dir.join(&name);
    fs::write(&tmp, text.as_bytes())?;
    fs::rename(&tmp, &final_path)?;
    debug!("Queued mid-turn input at {}", final_path.display());
    Ok(final_path)
}

/// Read and remove every queued message, oldest first.
///
/// Never returns an error: a mailbox problem must not be able to fail the turn
/// it is attached to. Unreadable or malformed entries are logged and removed so
/// they cannot wedge the mailbox and be re-reported on every iteration.
///
/// Empty messages are dropped — injecting an empty user turn would waste a
/// round trip and read to the model as a truncation artifact.
pub fn drain(inbox_dir: &Path) -> Vec<QueuedInput> {
    let entries = match fs::read_dir(inbox_dir) {
        Ok(e) => e,
        // The overwhelmingly common case: no inbox at all, because nothing was
        // queued. Not worth a log line per iteration.
        Err(_) => return Vec::new(),
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some(MESSAGE_EXT)
        })
        .collect();

    // Filename encodes creation time, so lexical order == send order.
    paths.sort();

    let mut out = Vec::new();
    for path in paths {
        match read_message(&path) {
            Ok(text) => {
                // Remove before yielding: at-most-once. See module docs.
                if let Err(e) = fs::remove_file(&path) {
                    warn!(
                        "Could not remove drained input {}: {} — skipping to avoid a duplicate injection",
                        path.display(),
                        e
                    );
                    continue;
                }
                if text.trim().is_empty() {
                    debug!("Dropping empty queued input {}", path.display());
                    continue;
                }
                debug!("Drained mid-turn input from {}", path.display());
                out.push(QueuedInput { text, source: path });
            }
            Err(e) => {
                warn!(
                    "Discarding unreadable queued input {}: {}",
                    path.display(),
                    e
                );
                // Remove it, or every subsequent drain re-reads the same bad file.
                let _ = fs::remove_file(&path);
            }
        }
    }
    out
}

/// Whether anything is currently queued, without consuming it.
pub fn has_pending(inbox_dir: &Path) -> bool {
    match fs::read_dir(inbox_dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).any(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some(MESSAGE_EXT)
        }),
        Err(_) => false,
    }
}

/// Read one message file, lossily decoding and truncating over the size cap.
///
/// Lossy rather than strict: a mangled byte should cost a replacement char, not
/// the whole interjection.
fn read_message(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let bytes = if bytes.len() > MAX_MESSAGE_BYTES {
        warn!(
            "Queued input {} is {} bytes, truncating to {}",
            path.display(),
            bytes.len(),
            MAX_MESSAGE_BYTES
        );
        // Truncate on a char boundary — from_utf8_lossy would otherwise turn a
        // split multi-byte char into a replacement character.
        let mut end = MAX_MESSAGE_BYTES;
        while end > 0 && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
            end -= 1;
        }
        &bytes[..end]
    } else {
        &bytes[..]
    };
    Ok(String::from_utf8_lossy(bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_inbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("g3_inbox_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn drains_in_send_order_and_empties_the_dir() {
        let dir = tmp_inbox("order");
        enqueue(&dir, "first").unwrap();
        enqueue(&dir, "second").unwrap();
        enqueue(&dir, "third").unwrap();

        let drained = drain(&dir);
        let texts: Vec<&str> = drained.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);

        // Mailbox is empty afterwards, and a second drain yields nothing.
        assert!(drain(&dir).is_empty());
        assert!(!has_pending(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_inbox_drains_to_empty_without_error() {
        // The common case: most turns have no inbox directory at all.
        let dir = std::env::temp_dir().join("g3_inbox_test_definitely_absent_xyz");
        let _ = fs::remove_dir_all(&dir);
        assert!(drain(&dir).is_empty());
        assert!(!has_pending(&dir));
    }

    #[test]
    fn ignores_files_without_the_message_extension() {
        let dir = tmp_inbox("ext");
        enqueue(&dir, "real").unwrap();
        fs::write(dir.join("notes.txt"), "not a message").unwrap();
        fs::write(dir.join(".01.partial"), "half-written").unwrap();

        let drained = drain(&dir);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text, "real");
        // Non-message files are left strictly alone.
        assert!(dir.join("notes.txt").exists());
        assert!(dir.join(".01.partial").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_and_whitespace_messages_are_dropped_not_injected() {
        let dir = tmp_inbox("empty");
        enqueue(&dir, "").unwrap();
        enqueue(&dir, "   \n\t ").unwrap();
        enqueue(&dir, "real content").unwrap();

        let drained = drain(&dir);
        assert_eq!(drained.len(), 1, "only the non-empty message should inject");
        assert_eq!(drained[0].text, "real content");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_file_is_discarded_and_does_not_wedge_the_mailbox() {
        let dir = tmp_inbox("garbage");
        // Invalid UTF-8 decodes lossily rather than failing the drain.
        fs::write(dir.join(format!("{:013}_{:06}.msg", 1, 0)), [0xff, 0xfe, 0x00]).unwrap();
        enqueue(&dir, "survivor").unwrap();

        let drained = drain(&dir);
        // The good message must still arrive.
        assert!(drained.iter().any(|q| q.text == "survivor"));
        // And nothing is left behind to be re-reported next iteration.
        assert!(!has_pending(&dir));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_message_is_truncated_not_rejected() {
        let dir = tmp_inbox("oversize");
        let huge = "x".repeat(MAX_MESSAGE_BYTES + 5_000);
        enqueue(&dir, &huge).unwrap();

        let drained = drain(&dir);
        assert_eq!(drained.len(), 1, "oversized message must still be delivered");
        assert!(drained[0].text.len() <= MAX_MESSAGE_BYTES);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncation_respects_utf8_char_boundaries() {
        let dir = tmp_inbox("utf8");
        // Multi-byte chars straddling the cap must not produce replacement chars.
        let msg = "é".repeat(MAX_MESSAGE_BYTES);
        enqueue(&dir, &msg).unwrap();

        let drained = drain(&dir);
        assert_eq!(drained.len(), 1);
        assert!(
            !drained[0].text.contains('\u{FFFD}'),
            "truncation split a multi-byte character"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn filenames_are_unique_within_the_same_millisecond() {
        // Two sends in the same millisecond must not collide, or one is lost.
        let a = generate_filename();
        let b = generate_filename();
        assert_ne!(a, b);
        // And they sort in generation order.
        assert!(a < b, "{} should sort before {}", a, b);
    }

    #[test]
    fn concurrent_enqueue_loses_nothing() {
        let dir = tmp_inbox("concurrent");
        let mut handles = Vec::new();
        for i in 0..8 {
            let d = dir.clone();
            handles.push(std::thread::spawn(move || {
                enqueue(&d, &format!("msg{}", i)).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let drained = drain(&dir);
        assert_eq!(drained.len(), 8, "concurrent writers clobbered each other");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_write_is_never_observed_mid_flight() {
        // enqueue writes a dotfile then renames, so a concurrent drain sees
        // either nothing or the whole message — never a prefix.
        let dir = tmp_inbox("atomic");
        let big = "y".repeat(32 * 1024);
        let d = dir.clone();
        let writer = std::thread::spawn(move || {
            for _ in 0..20 {
                enqueue(&d, &big).unwrap();
            }
        });
        let mut seen = Vec::new();
        for _ in 0..40 {
            seen.extend(drain(&dir));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        writer.join().unwrap();
        seen.extend(drain(&dir));
        assert_eq!(seen.len(), 20, "a message was lost or double-read");
        for q in &seen {
            assert_eq!(q.text.len(), 32 * 1024, "observed a partially-written message");
        }
        fs::remove_dir_all(&dir).ok();
    }
}
