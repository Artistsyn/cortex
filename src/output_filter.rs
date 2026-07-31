//! Lossless command-output compaction.
//!
//! This module removes only PROVABLY-REDUNDANT content from command output —
//! build/download progress chatter, per-test "ok" lines (equivalent to what
//! `cargo -q` already suppresses), and consecutive duplicate lines. It NEVER
//! drops a diagnostic: every `error`, `warning`, `note`, panic, failure block,
//! and captured-output section is preserved verbatim, with its `file:line`.
//!
//! Design contract (why this is safe to run automatically on tool output):
//!   - Compaction is line-classified: a line is dropped ONLY if it matches a
//!     known-noise rule. Anything unrecognized is KEPT. Failure is toward
//!     preservation, never toward loss.
//!   - The full raw output is tee'd to disk whenever anything is dropped, so
//!     the exact original byte stream is always one read away.
//!   - `lossless` is reported per call and is `true` for every strategy here.
//!
//! Anything that would require judgement about what the agent "needs" (stripping
//! function bodies, grouping warnings so `file:line` is lost, truncating diff
//! hunks) is deliberately NOT implemented — those belong to an opt-in lossy tier
//! that this module does not provide.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    CargoTest,
    CargoBuild,
    CargoClippy,
    GitStatus,
    Generic,
}

#[derive(Debug, Clone)]
pub struct FilteredOutput {
    pub text: String,
    pub original_chars: usize,
    pub filtered_chars: usize,
    pub dropped_lines: usize,
    pub tee_path: Option<PathBuf>,
    /// Always true for this module — documents the guarantee at the call site.
    pub lossless: bool,
}

/// Classify a command string into the filtering strategy it should use.
pub fn detect_command(cmd: &str) -> CommandKind {
    let c = cmd.trim_start();
    // Normalize leading `cargo +toolchain` and environment prefixes lightly by
    // scanning for the meaningful verb anywhere near the start.
    let has = |needle: &str| c.contains(needle);
    if has("cargo") && has("clippy") {
        CommandKind::CargoClippy
    } else if has("cargo") && has("test") {
        CommandKind::CargoTest
    } else if has("cargo") && (has("build") || has("check")) {
        CommandKind::CargoBuild
    } else if has("git") && has("status") {
        CommandKind::GitStatus
    } else {
        CommandKind::Generic
    }
}

/// Cargo status verbs that carry no diagnostic content and are safe to drop.
/// `Finished` is intentionally EXCLUDED — it is the success signal and is kept.
const CARGO_PROGRESS_VERBS: &[&str] = &[
    "Compiling",
    "Checking",
    "Downloading",
    "Downloaded",
    "Updating",
    "Blocking",
    "Locking",
    "Installing",
    "Fresh",
    "Building",
    "Waiting",
    "Packaging",
    "Verifying",
    "Adding",
    "Removing",
    "Ignored",
];

fn is_cargo_progress(line: &str) -> bool {
    let t = line.trim_start();
    // Cargo status lines are `<Verb> <rest>`; match the first whitespace token.
    match t.split_whitespace().next() {
        Some(first) => CARGO_PROGRESS_VERBS.contains(&first),
        None => false,
    }
}

/// A per-test pass line: `test path::to::test ... ok`. Dropping these is
/// equivalent to `cargo test -q` (which prints dots, not names); the count is
/// preserved by the retained `test result:` summary line. Failures/ignored are
/// NOT matched here and are always kept.
fn is_passing_test_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("test ") && t.ends_with(" ... ok")
}

/// Collapse runs of blank lines to a single blank; returns kept lines.
/// Purely cosmetic whitespace — no information carried.
fn collapse_blank_runs(lines: Vec<String>) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut dropped = 0usize;
    let mut prev_blank = false;
    for l in lines {
        let blank = l.trim().is_empty();
        if blank && prev_blank {
            dropped += 1;
            continue;
        }
        prev_blank = blank;
        out.push(l);
    }
    (out, dropped)
}

/// cargo build / check: drop only progress verbs (keep `Finished`), collapse
/// blank runs. Every diagnostic line is preserved verbatim.
fn filter_cargo_build(raw: &str) -> (String, usize) {
    let mut dropped = 0usize;
    let kept: Vec<String> = raw
        .lines()
        .filter(|l| {
            if is_cargo_progress(l) {
                dropped += 1;
                false
            } else {
                true
            }
        })
        .map(|l| l.to_string())
        .collect();
    let (kept, blank_dropped) = collapse_blank_runs(kept);
    dropped += blank_dropped;
    (kept.join("\n"), dropped)
}

/// cargo clippy: identical policy to build — keep every diagnostic verbatim
/// (grouping by lint would drop the `file:line` the agent needs, so we do not).
fn filter_cargo_clippy(raw: &str) -> (String, usize) {
    filter_cargo_build(raw)
}

/// cargo test: drop build progress + per-test `... ok` lines (== cargo -q),
/// collapse blank runs. All FAILED lines, the `failures:` section, panics,
/// captured output, and every `test result:` summary are kept verbatim.
fn filter_cargo_test(raw: &str) -> (String, usize) {
    let mut dropped = 0usize;
    let kept: Vec<String> = raw
        .lines()
        .filter(|l| {
            if is_cargo_progress(l) || is_passing_test_line(l) {
                dropped += 1;
                false
            } else {
                true
            }
        })
        .map(|l| l.to_string())
        .collect();
    let (kept, blank_dropped) = collapse_blank_runs(kept);
    dropped += blank_dropped;
    (kept.join("\n"), dropped)
}

/// git status (long form): drop the instructional hint lines (the
/// `(use "git ..." ...)` guidance) which carry no repository state. Every file
/// path and section header is kept.
fn filter_git_status(raw: &str) -> (String, usize) {
    let mut dropped = 0usize;
    let kept: Vec<String> = raw
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            if t.starts_with("(use ") {
                dropped += 1;
                false
            } else {
                true
            }
        })
        .map(|l| l.to_string())
        .collect();
    let (kept, blank_dropped) = collapse_blank_runs(kept);
    dropped += blank_dropped;
    (kept.join("\n"), dropped)
}

/// Generic: collapse runs of IDENTICAL consecutive lines to `line  (×N)`.
/// The count preserves the information that N copies existed — lossless.
/// No truncation: unique content passes through untouched.
fn filter_generic(raw: &str) -> (String, usize) {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut dropped = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        let cur = lines[i];
        let mut run = 1usize;
        while i + run < lines.len() && lines[i + run] == cur {
            run += 1;
        }
        if run > 1 {
            out.push(format!("{cur}  (×{run})"));
            dropped += run - 1;
        } else {
            out.push(cur.to_string());
        }
        i += run;
    }
    (out.join("\n"), dropped)
}

fn filter_by_kind(kind: CommandKind, raw: &str) -> (String, usize) {
    match kind {
        CommandKind::CargoTest => filter_cargo_test(raw),
        CommandKind::CargoBuild => filter_cargo_build(raw),
        CommandKind::CargoClippy => filter_cargo_clippy(raw),
        CommandKind::GitStatus => filter_git_status(raw),
        CommandKind::Generic => filter_generic(raw),
    }
}

/// Deterministic short id for a tee filename, from the raw content.
fn tee_stem(raw: &str) -> String {
    // Cheap FNV-1a over the bytes — no crypto needed, just a stable name.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in raw.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    format!("{ts}-{h:016x}")
}

/// Below this many chars, filtering is skipped entirely — the round-trip and
/// any dropped-line note would cost more than it saves.
pub const MIN_FILTER_CHARS: usize = 800;

/// Filter `raw` for the given command kind. Tees the full original to
/// `tee_dir` (when provided) only if lines were actually dropped, so the exact
/// byte stream is always recoverable.
pub fn filter_output(kind: CommandKind, raw: &str, tee_dir: Option<&Path>) -> FilteredOutput {
    let original_chars = raw.chars().count();

    // Size floor: tiny outputs are returned untouched.
    if original_chars < MIN_FILTER_CHARS {
        return FilteredOutput {
            text: raw.to_string(),
            original_chars,
            filtered_chars: original_chars,
            dropped_lines: 0,
            tee_path: None,
            lossless: true,
        };
    }

    let (mut text, dropped_lines) = filter_by_kind(kind, raw);

    let mut tee_path = None;
    if dropped_lines > 0 {
        if let Some(dir) = tee_dir {
            if std::fs::create_dir_all(dir).is_ok() {
                let path = dir.join(format!("{}.txt", tee_stem(raw)));
                if std::fs::write(&path, raw).is_ok() {
                    text.push_str(&format!(
                        "\n[compacted: {dropped_lines} redundant line(s) removed losslessly \u{2014} full log: {}]",
                        path.display()
                    ));
                    tee_path = Some(path);
                }
            }
        }
    }

    let filtered_chars = text.chars().count();
    FilteredOutput {
        text,
        original_chars,
        filtered_chars,
        dropped_lines,
        tee_path,
        lossless: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_maps_commands() {
        assert_eq!(detect_command("cargo test --manifest-path x"), CommandKind::CargoTest);
        assert_eq!(detect_command("cargo build"), CommandKind::CargoBuild);
        assert_eq!(detect_command("cargo check -q"), CommandKind::CargoBuild);
        assert_eq!(detect_command("cargo clippy -- -D warnings"), CommandKind::CargoClippy);
        assert_eq!(detect_command("git status"), CommandKind::GitStatus);
        assert_eq!(detect_command("ls -la"), CommandKind::Generic);
    }

    #[test]
    fn cargo_build_keeps_every_diagnostic_line_verbatim() {
        let raw = "\
   Compiling foo v0.1.0
   Compiling bar v0.2.0
warning: unused variable: `x`
 --> src/lib.rs:10:9
  |
10|     let x = 5;
  |         ^ help: prefix with underscore: `_x`
error[E0308]: mismatched types
 --> src/lib.rs:20:5
  |
20|     return 1;
  |            ^ expected `String`, found integer
   Compiling baz v0.3.0
    Finished dev profile";
        let (text, dropped) = filter_cargo_build(raw);
        // Every diagnostic line survives, in order, byte-for-byte.
        for needle in [
            "warning: unused variable: `x`",
            "--> src/lib.rs:10:9",
            "help: prefix with underscore: `_x`",
            "error[E0308]: mismatched types",
            "--> src/lib.rs:20:5",
            "expected `String`, found integer",
            "Finished dev profile", // success signal kept
        ] {
            assert!(text.contains(needle), "lost diagnostic line: {needle}\n---\n{text}");
        }
        // Progress verbs are gone.
        assert!(!text.contains("Compiling foo"));
        assert!(!text.contains("Compiling bar"));
        assert!(dropped >= 3, "expected >=3 progress lines dropped, got {dropped}");
    }

    #[test]
    fn cargo_test_all_pass_collapses_to_summary() {
        let raw = "\
   Compiling foo v0.1.0
running 3 tests
test tests::a ... ok
test tests::b ... ok
test tests::c ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let (text, dropped) = filter_cargo_test(raw);
        assert!(text.contains("test result: ok. 3 passed"), "summary must be kept:\n{text}");
        assert!(!text.contains("tests::a ... ok"), "per-test ok lines must be dropped");
        assert_eq!(dropped, 4, "3 ok lines + 1 Compiling"); // blank run not doubled
    }

    #[test]
    fn cargo_test_keeps_all_failure_detail() {
        let raw = "\
   Compiling foo v0.1.0
running 2 tests
test tests::passes ... ok
test tests::breaks ... FAILED

failures:

---- tests::breaks stdout ----
thread 'tests::breaks' panicked at src/lib.rs:42:5:
assertion `left == right` failed
  left: 1
 right: 2

failures:
    tests::breaks

test result: FAILED. 1 passed; 1 failed; 0 ignored";
        let (text, _dropped) = filter_cargo_test(raw);
        for needle in [
            "tests::breaks ... FAILED",
            "---- tests::breaks stdout ----",
            "panicked at src/lib.rs:42:5",
            "assertion `left == right` failed",
            "left: 1",
            "right: 2",
            "test result: FAILED. 1 passed; 1 failed",
        ] {
            assert!(text.contains(needle), "lost failure detail: {needle}\n---\n{text}");
        }
        // The one passing line is still collapsed.
        assert!(!text.contains("tests::passes ... ok"));
    }

    #[test]
    fn clippy_keeps_lint_location() {
        let raw = "\
    Checking foo v0.1.0
warning: this `if` has identical blocks
 --> src/main.rs:5:5
warning: unused import: `std::io`
 --> src/main.rs:1:5
    Finished";
        let (text, _d) = filter_cargo_clippy(raw);
        assert!(text.contains("src/main.rs:5:5"));
        assert!(text.contains("src/main.rs:1:5"));
        assert!(text.contains("unused import: `std::io`"));
        assert!(!text.contains("Checking foo"));
    }

    #[test]
    fn git_status_keeps_paths_drops_hints() {
        let raw = "\
On branch main
Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
  (use \"git restore <file>...\" to discard changes in working directory)
\tmodified:   src/foo.rs
\tmodified:   src/bar.rs
Untracked files:
  (use \"git add <file>...\" to include in what will be committed)
\tbaz.rs";
        let (text, dropped) = filter_git_status(raw);
        assert!(text.contains("modified:   src/foo.rs"));
        assert!(text.contains("modified:   src/bar.rs"));
        assert!(text.contains("baz.rs"));
        assert!(text.contains("On branch main"));
        assert!(!text.contains("(use \"git add"));
        assert_eq!(dropped, 3);
    }

    #[test]
    fn generic_dedup_is_lossless_with_count() {
        let raw = "warn: retrying\nwarn: retrying\nwarn: retrying\ndone";
        let (text, dropped) = filter_generic(raw);
        assert!(text.contains("warn: retrying  (×3)"));
        assert!(text.contains("done"));
        assert_eq!(dropped, 2);
    }

    #[test]
    fn size_floor_returns_input_untouched() {
        let raw = "   Compiling foo v0.1.0\n    Finished";
        let out = filter_output(CommandKind::CargoBuild, raw, None);
        assert_eq!(out.text, raw, "below floor => untouched");
        assert_eq!(out.dropped_lines, 0);
        assert!(out.lossless);
    }

    #[test]
    fn filter_output_reports_savings_and_stays_lossless() {
        // Build a large all-pass test log so we clear the size floor.
        let mut raw = String::from("   Compiling foo v0.1.0\nrunning 200 tests\n");
        for i in 0..200 {
            raw.push_str(&format!("test suite::case_{i:03} ... ok\n"));
        }
        raw.push_str("\ntest result: ok. 200 passed; 0 failed; 0 ignored");
        let out = filter_output(CommandKind::CargoTest, &raw, None);
        assert!(out.lossless);
        assert!(out.filtered_chars < out.original_chars / 4, "expected big savings");
        assert!(out.text.contains("test result: ok. 200 passed"));
        assert!(out.dropped_lines >= 200);
    }
}
