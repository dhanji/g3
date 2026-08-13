//! Cross-language mailbox compatibility.
//!
//! butler.app (Python) writes queued messages; g3 (Rust) drains them. The two
//! sides share only a filename convention and a directory, so nothing in either
//! codebase forces them to agree — a change to the Python format would break
//! draining silently, with messages piling up in the inbox unread.
//!
//! These tests write files exactly as `butler.app/streaming.py::enqueue_mid_turn`
//! does and assert the Rust drain accepts them.

use g3_core::pending_input;
use std::fs;
use std::path::PathBuf;

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "g3_xlang_inbox_{}_{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Mirrors the Python side's filename construction:
///   f"{int(time.time() * 1000):013d}_{next(counter):06d}.msg"
fn python_style_name(millis: u64, counter: u32) -> String {
    format!("{:013}_{:06}.msg", millis, counter)
}

#[test]
fn drains_files_written_in_the_python_filename_format() {
    let dir = tmp_dir("format");
    // Realistic millis value (13 digits, as produced by time.time() * 1000).
    fs::write(dir.join(python_style_name(1_754_000_000_000, 0)), "first").unwrap();
    fs::write(dir.join(python_style_name(1_754_000_000_001, 1)), "second").unwrap();

    let drained = pending_input::drain(&dir);
    let texts: Vec<&str> = drained.iter().map(|q| q.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["first", "second"],
        "Rust drain did not accept Python-written filenames"
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn python_counter_breaks_same_millisecond_ties_in_send_order() {
    let dir = tmp_dir("ties");
    // Both sent inside the same millisecond — ordering must come from the
    // counter, not from readdir order (which is arbitrary).
    fs::write(dir.join(python_style_name(1_754_000_000_000, 7)), "later").unwrap();
    fs::write(dir.join(python_style_name(1_754_000_000_000, 3)), "earlier").unwrap();

    let drained = pending_input::drain(&dir);
    let texts: Vec<&str> = drained.iter().map(|q| q.text.as_str()).collect();
    assert_eq!(texts, vec!["earlier", "later"]);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn python_partial_dotfile_is_not_drained() {
    let dir = tmp_dir("partial");
    // What the Python side leaves on disk mid-write, before its rename.
    let name = python_style_name(1_754_000_000_000, 0);
    fs::write(dir.join(format!(".{}.partial", name)), "half written").unwrap();

    let drained = pending_input::drain(&dir);
    assert!(
        drained.is_empty(),
        "an in-progress Python write was drained as a message"
    );
    // And it is left alone for the rename to complete.
    assert!(dir.join(format!(".{}.partial", name)).exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn utf8_written_by_python_survives_the_drain() {
    let dir = tmp_dir("utf8");
    // Emoji plus Devanagari — both routine in this workspace.
    let msg = "actually — check राजी's email 🎩";
    fs::write(dir.join(python_style_name(1_754_000_000_000, 0)), msg).unwrap();

    let drained = pending_input::drain(&dir);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, msg);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn zero_padding_is_load_bearing_for_ordering() {
    // Documents WHY the Python format zero-pads. Without padding, lexical sort
    // diverges from numeric sort and messages drain out of order.
    let unpadded = vec!["9_0.msg", "10_0.msg"];
    let mut sorted_unpadded = unpadded.clone();
    sorted_unpadded.sort();
    assert_eq!(
        sorted_unpadded,
        vec!["10_0.msg", "9_0.msg"],
        "unpadded names sort wrong — this is the bug padding prevents"
    );

    let padded = vec![python_style_name(9, 0), python_style_name(10, 0)];
    let mut sorted_padded = padded.clone();
    sorted_padded.sort();
    assert_eq!(sorted_padded, padded, "padded names must sort numerically");
}
