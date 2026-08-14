//! Tests for the `--memory <path>` override and the single memory-path resolver.
//!
//! The invariant under test is subtle but load-bearing: g3 resolves the memory
//! file location in TWO places — the read path (g3-cli loading memory into the
//! system prompt at startup) and the write path (the `remember` tool). If those
//! ever disagree, memory forks silently: reads come from one file, writes land in
//! another, and NEITHER side reports an error. The user just observes that
//! remembered facts "don't stick".
//!
//! So these tests assert not only that the flag works, but that exactly one
//! function decides the answer.

use g3_core::tools::memory::{resolve_memory_path, DEFAULT_MEMORY_RELATIVE_PATH};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Happy path: the override is honoured, and read/write agree
// ---------------------------------------------------------------------------

#[test]
fn test_override_absolute_path_is_used_verbatim() {
    let resolved = resolve_memory_path(Some("/tmp/workspace"), Some("/var/data/memory.md"));
    assert_eq!(resolved, PathBuf::from("/var/data/memory.md"));
}

#[test]
fn test_override_expands_leading_tilde() {
    let resolved = resolve_memory_path(Some("/tmp/workspace"), Some("~/icloud/butler/memory.md"));
    let home = dirs::home_dir().expect("home dir");

    assert_eq!(resolved, home.join("icloud/butler/memory.md"));
    assert!(
        resolved.is_absolute(),
        "a tilde path must resolve to an absolute path, got {:?}",
        resolved
    );
    assert!(
        !resolved.to_string_lossy().contains('~'),
        "tilde must be expanded, not passed through literally: {:?}",
        resolved
    );
}

/// The whole point of the refactor: one resolver, so both call sites land on the
/// same file. Calling it twice with the same inputs (as the read and write paths
/// each do) must produce identical results.
#[test]
fn test_read_and_write_paths_agree_under_override() {
    let workspace = Some("/tmp/workspace");
    let override_path = Some("~/icloud/butler/memory.md");

    let read_side = resolve_memory_path(workspace, override_path);
    let write_side = resolve_memory_path(workspace, override_path);

    assert_eq!(
        read_side, write_side,
        "read and write paths must resolve identically or memory forks silently"
    );
}

/// A relative override is resolved against the workspace, NOT the process CWD.
/// If it were CWD-relative, a duty that chdirs (several butler harnesses do)
/// would read and write different files.
#[test]
fn test_relative_override_resolves_against_workspace_not_cwd() {
    let resolved = resolve_memory_path(Some("/tmp/workspace"), Some("notes/mem.md"));
    assert_eq!(resolved, PathBuf::from("/tmp/workspace/notes/mem.md"));

    // Same relative override, different workspace => different absolute path.
    let elsewhere = resolve_memory_path(Some("/other/ws"), Some("notes/mem.md"));
    assert_ne!(resolved, elsewhere);
}

// ---------------------------------------------------------------------------
// Boundary: no flag must be byte-identical to historical behaviour
// ---------------------------------------------------------------------------

#[test]
fn test_no_override_uses_default_analysis_memory_md() {
    let resolved = resolve_memory_path(Some("/tmp/workspace"), None);
    assert_eq!(resolved, PathBuf::from("/tmp/workspace/analysis/memory.md"));
}

/// Other g3 projects must be unaffected by this feature. The default is exactly
/// `<workspace>/analysis/memory.md` and the published constant must match it.
#[test]
fn test_default_relative_path_constant_matches_resolver() {
    let workspace = "/tmp/workspace";
    let resolved = resolve_memory_path(Some(workspace), None);
    let expected = PathBuf::from(workspace).join(DEFAULT_MEMORY_RELATIVE_PATH);

    assert_eq!(
        resolved, expected,
        "DEFAULT_MEMORY_RELATIVE_PATH must describe what the resolver actually does"
    );
}

#[test]
fn test_no_workspace_falls_back_to_cwd() {
    let resolved = resolve_memory_path(None, None);
    let cwd = std::env::current_dir().expect("cwd");
    assert_eq!(resolved, cwd.join("analysis").join("memory.md"));
}

// ---------------------------------------------------------------------------
// Negative: degenerate overrides must not silently become a weird path
// ---------------------------------------------------------------------------

/// An empty or whitespace-only `--memory ""` is a user/harness error (e.g. an
/// unset shell variable expanding to nothing). It must fall back to the default
/// rather than resolving to the workspace directory ITSELF — which would make g3
/// try to read a directory as memory, and worse, WRITE memory over it.
#[test]
fn test_empty_override_falls_back_to_default() {
    for degenerate in ["", "   ", "\t", "\n"] {
        let resolved = resolve_memory_path(Some("/tmp/workspace"), Some(degenerate));
        assert_eq!(
            resolved,
            PathBuf::from("/tmp/workspace/analysis/memory.md"),
            "degenerate override {:?} must fall back to the default, not to the workspace dir",
            degenerate
        );
        assert_ne!(
            resolved,
            PathBuf::from("/tmp/workspace"),
            "resolver must never return the workspace directory itself"
        );
    }
}

/// Surrounding whitespace from shell interpolation must not create a distinct
/// path — otherwise read and write could disagree if one side happened to trim.
#[test]
fn test_override_is_trimmed() {
    let padded = resolve_memory_path(Some("/tmp/ws"), Some("  /var/mem.md  "));
    let clean = resolve_memory_path(Some("/tmp/ws"), Some("/var/mem.md"));
    assert_eq!(padded, clean);
}

/// A nonexistent override path is NOT an error at resolve time. The read path
/// degrades to "no memory" (same as a missing analysis/memory.md) and the write
/// path creates the file. Resolution must stay pure and infallible.
#[test]
fn test_nonexistent_override_resolves_without_error() {
    let resolved = resolve_memory_path(Some("/tmp/ws"), Some("/nonexistent/dir/memory.md"));
    assert_eq!(resolved, PathBuf::from("/nonexistent/dir/memory.md"));
    assert!(!resolved.exists(), "fixture assumption: path should not exist");
}

// ---------------------------------------------------------------------------
// Structural: exactly one place may decide the memory path
// ---------------------------------------------------------------------------

/// A source-inspection test, deliberately.
///
/// The bug this guards against is an OMISSION: someone adds a third consumer of
/// workspace memory and hand-joins `analysis/memory.md` instead of calling the
/// resolver. No behavioural test catches that, because the new site would look
/// correct in isolation — it only breaks in combination with `--memory`.
#[test]
fn test_no_hardcoded_memory_path_outside_resolver() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf();

    let mut offenders = Vec::new();

    for crate_name in ["g3-core", "g3-cli"] {
        let src = repo_root.join("crates").join(crate_name).join("src");
        collect_hardcoded_memory_paths(&src, &mut offenders);
    }

    assert!(
        offenders.is_empty(),
        "memory.md path must only be constructed inside resolve_memory_path(); \
         found hardcoded construction at:\n  {}",
        offenders.join("\n  ")
    );
}

fn collect_hardcoded_memory_paths(dir: &PathBuf, offenders: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_hardcoded_memory_paths(&path, offenders);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // The resolver itself is the one legal home for this construction.
        let is_resolver_file = path.ends_with("tools/memory.rs");

        for (idx, line) in content.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);

            // Pattern: joining "analysis" then "memory.md", in either style.
            let joins_analysis_memory = code.contains(r#"join("analysis")"#)
                && (code.contains(r#"join("memory.md")"#) || content.contains(r#"join("memory.md")"#));
            let joins_combined = code.contains(r#""analysis/memory.md""#);

            if !(joins_analysis_memory || joins_combined) {
                continue;
            }

            // Inside the resolver, the literal is expected (that IS the default).
            if is_resolver_file {
                continue;
            }

            offenders.push(format!(
                "{}:{}: {}",
                path.strip_prefix(dir).unwrap_or(&path).display(),
                idx + 1,
                code.trim()
            ));
        }
    }
}
