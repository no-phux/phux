//! Grep gate (PHA-406 L21): CLI verbs that hand-roll wire frames belong in a
//! `phux-client` module (`Connection::connect` -> `request` -> typed result
//! -> typed error, the `send_keys.rs` shape), not in `crates/phux/src/commands/`.
//!
//! This walks every `.rs` file under `src/commands/` and fails if any of the
//! raw-wire markers (see [`MARKERS`] and [`command_marker_hits`]) appear in
//! its *production* code outside an explicit, per-file-capped allowance.
//!
//! Two exclusions keep this honest rather than noisy:
//!
//! - **Comments and string literals never count.** [`strip_comments_and_strings`]
//!   blanks out `//` and `/* */` comments and the *contents* of `"..."`
//!   string/byte-string literals (keeping line/column layout intact) before
//!   anything else runs. Without this, `eprintln!("... {message}")` — which
//!   is everywhere in this tree — would corrupt brace counting and could
//!   register a marker that only exists inside a diagnostic string.
//! - **`mod tests { ... }` bodies never count.** A scripted mock server in a
//!   unit test legitimately builds frames and opens fake connections to play
//!   a peer; that is not the anti-pattern this gate polices.
//!
//! [`ALLOWLIST`] is the explicit, precise residue: every file that still owns
//! a raw connection, frame construction, or request round trip, why, and a
//! **pinned maximum hit count**. A file can drop below its pin for free (the
//! comment there goes stale, which is fine); it can never rise above it
//! without a reviewer looking at the new code and raising the number
//! deliberately. Migrating a file to zero and deleting its entry is a strict
//! improvement.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test-only file-scanning gate: a read failure here is a test-harness bug, \
              not a condition to recover from"
)]

use std::path::{Path, PathBuf};

/// Files (relative to `crates/phux/src/commands/`) allowed to still touch a
/// raw connection, a wire frame constructor, or a raw request round trip in
/// production code: `(path, max_hits, why)`.
///
/// `max_hits` is a ceiling, not a target: the actual count only needs to be
/// at or below it. Raise it only alongside a diff a reviewer can see and
/// agree cannot move yet; lower it (or delete the row) freely as files are
/// migrated.
const ALLOWLIST: &[(&str, usize, &str)] = &[
    (
        "mod.rs",
        27,
        "owns the shared `command_on`/`request_command` helpers every verb \
         (migrated or not) calls through — the one generic connect + request \
         round trip, not a per-verb hand-roll — plus `verb_remote` and \
         `socketless_verb`, which match on *this crate's own* `Command` (the \
         clap subcommand enum, `pub(crate) enum Command` below `Cli`), a \
         same-named but unrelated type to `phux_protocol`'s wire `Command`. \
         Both contribute to the `Command::` count below.",
    ),
    (
        "server_target.rs",
        2,
        "owns the one real `Connection::connect`/`connect_dial` a `ServerSpec` \
         resolves to; every migrated verb in this lane reaches it through \
         `ServerTarget::connect`, never directly.",
    ),
    (
        "spawn.rs",
        3,
        "the plain `phux spawn` wire round trip is fully in \
         `phux_client::spawn`; the agent-session provenance write and its \
         `KILL_RESOURCE` rollback (formerly this file's residue) are fully \
         in `phux_client::agent_session_record::spawn_with_agent_session_on` \
         (L21b) — no `Connection::connect`, `.request(`, `.send(&`, \
         `request_metadata(`, `request_spawn(`, or `command_on(` remains in \
         this file. The 3 remaining hits are request-shaping, not round \
         trips: `run_spawn`'s own `FrameKind::SpawnResource` literal, and \
         `dispatch_spawn_placed`'s `Command::GetState` value passed to the \
         shared `request_command` helper plus its `FrameKind::SpawnResource` \
         destructure to stamp `owner_terminal` before the placed spawn.",
    ),
    (
        "launch.rs",
        1,
        "shares `spawn.rs`'s `dispatch_spawn`/`dispatch_spawn_placed` and \
         builds its own `FrameKind::SpawnResource` request literal from a \
         resolved integration, same as `phux spawn`'s own literal \
         (request-shaping, not a round trip); the wire round trip itself is \
         fully delegated (L21b).",
    ),
    (
        "spatial.rs",
        3,
        "not migrated in this pass. The `GET_STATE` site was in this lane's \
         goal, but `phux_client::state::get_state_on` classifies a \
         `CommandResult::Error` refusal differently \
         (`AttachError::Refused`) than `read_snapshot`'s current \
         `AttachError::Protocol(explain_unexpected(..))`, which would change \
         a refusal's exact error text; left for a follow-up that also owns \
         that reconciliation. `spatial.rs` is also L18's file — minimal \
         hunks only.",
    ),
    (
        "play.rs",
        1,
        "not in this lane's write scope (`phux play`'s own \
         `SPAWN_RESOURCE`); left as-is.",
    ),
    (
        "bootstrap.rs",
        3,
        "not in this lane's write scope (`phux bootstrap`'s `OPEN_LISTENER`); \
         left as-is.",
    ),
    (
        "config.rs",
        4,
        "not in this lane's write scope (`config set`'s `SET_METADATA`); left \
         as-is.",
    ),
    (
        "perf.rs",
        1,
        "not in this lane's write scope (`GET_PERF`); left as-is.",
    ),
    (
        "status.rs",
        1,
        "not in this lane's write scope (`GET_STATE` for `phux status`); left \
         as-is.",
    ),
    (
        "enroll.rs",
        1,
        "not in this lane's write scope (pairing/enrollment dials); left as-is.",
    ),
    (
        "whoami.rs",
        1,
        "not in this lane's write scope (`GET_METADATA` of `phux.whoami/v1`); \
         left as-is.",
    ),
    (
        "workspace/archive.rs",
        3,
        "not in this lane's write scope (`workspace restore`'s session \
         create); left as-is.",
    ),
    (
        "agent/answer.rs",
        1,
        "not in this lane's write scope (`phux agent answer`); left as-is.",
    ),
    (
        "agent/report_state.rs",
        2,
        "not in this lane's write scope (`phux agent report-state`'s \
         `REPORT_AGENT_STATE`); left as-is.",
    ),
    (
        "agent/resource_session.rs",
        1,
        "not in this lane's write scope (`phux agent session` internals); \
         left as-is.",
    ),
    (
        "agent/start.rs",
        8,
        "not in this lane's write scope (`phux agent start`); left as-is.",
    ),
];

/// Literal markers of a hand-rolled wire call, checked with `str::contains`
/// against the comment/string-stripped production text.
///
/// `Command::` is handled separately by [`command_marker_hits`] because it
/// needs a word boundary and two exclusions (`std::process::Command`, and
/// its `::new(` constructor) that a bare substring cannot express.
const MARKERS: &[&str] = &[
    "Connection::connect",
    "FrameKind::",
    "WireCommand::",
    ".request(",
    "request_metadata(",
    "request_spawn(",
    "command_on(",
    ".send(&",
];

#[test]
fn cli_commands_open_no_raw_connections() {
    let commands_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    assert!(
        commands_dir.is_dir(),
        "expected {commands_dir:?} to exist; has the crate layout moved?"
    );

    let mut offenders = Vec::new();
    for path in rust_files(&commands_dir) {
        let relative = path
            .strip_prefix(&commands_dir)
            .expect("walked path is under commands_dir")
            .to_string_lossy()
            .replace('\\', "/");
        let allowed = ALLOWLIST
            .iter()
            .find(|(name, _, _)| *name == relative)
            .map(|(_, max_hits, _)| *max_hits);

        let content =
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {path:?}: {err}"));
        let hits = production_hits(&content);
        let count = hits.len();

        match allowed {
            None if count > 0 => {
                for (line_no, marker) in &hits {
                    offenders.push(format!("{relative}:{line_no}: {marker} (not on ALLOWLIST)"));
                }
            }
            Some(max_hits) if count > max_hits => {
                offenders.push(format!(
                    "{relative}: {count} hits exceeds its ALLOWLIST cap of {max_hits} \
                     (new hits: {})",
                    hits.iter()
                        .skip(max_hits)
                        .map(|(line_no, marker)| format!("{line_no}:{marker}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            _ => {}
        }
    }

    assert!(
        offenders.is_empty(),
        "these files hand-roll a wire connection, frame, or request outside phux-client, \
         beyond what ALLOWLIST accounts for:\n{}\n\n\
         Move the wire work into a phux-client module (the send_keys.rs shape: \
         Connection::connect -> request -> typed result -> typed error), or, if this \
         file is genuinely out of this lane's scope, raise its ALLOWLIST cap with a \
         reason a reviewer can check against the diff.",
        offenders.join("\n")
    );
}

#[test]
fn allowlist_entries_are_unique() {
    let mut names: Vec<&str> = ALLOWLIST.iter().map(|(name, _, _)| *name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "ALLOWLIST has a duplicate entry");
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|err| panic!("reading {}: {err}", current.display()));
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out
}

/// Reduce `content` to its "code-only" text for gate purposes: `//` and
/// `/* */` comments, and the *contents* of `"..."` string/byte-string
/// literals (including their escapes), are blanked out character-for-
/// character — never removed — so line numbers and brace positions stay
/// meaningful.
///
/// Deliberately simple and biased toward *not* stripping when unsure, since
/// under-stripping only costs an extra allowlist entry (safe) while
/// over-stripping could hide real hand-rolled wire code (unsafe):
/// - Block comments are tracked non-recursively; this codebase does not nest
///   them, and nesting would only leave *more* text visible, never less.
/// - Char literals (`'x'`) are not specially recognized, so a lifetime
///   (`'a`) is never mistaken for a string opener that could swallow real
///   code before the next `'`.
/// - Raw strings (`r"..."`, `r#"..."#`) are scanned as ordinary strings:
///   correct unless the body contains a literal `\` immediately before the
///   closing quote, which does not occur in this tree.
fn strip_comments_and_strings(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_line_comment {
            in_line_comment = c != '\n';
            out.push(if c == '\n' { '\n' } else { ' ' });
            continue;
        }
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
                out.push_str("  ");
            } else {
                out.push(if c == '\n' { '\n' } else { ' ' });
            }
            continue;
        }
        if in_string {
            if c == '\\' {
                out.push(' ');
                if let Some(&next) = chars.peek()
                    && next != '\n'
                {
                    chars.next();
                    out.push(' ');
                }
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            out.push(if c == '\n' { '\n' } else { ' ' });
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            in_line_comment = true;
            out.push_str("  ");
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            out.push_str("  ");
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// `(1-based line number, marker)` for every raw-wire marker found in
/// `content`'s production code: comments, string-literal contents (see
/// [`strip_comments_and_strings`]), and the body of any `mod tests { ... }`
/// block are excluded.
fn production_hits(content: &str) -> Vec<(usize, String)> {
    let code = strip_comments_and_strings(content);
    let lines: Vec<&str> = code.lines().collect();

    let mut hits = Vec::new();
    let mut skip_depth: i32 = 0;
    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        if skip_depth > 0 {
            skip_depth += brace_delta(line);
            continue;
        }
        if starts_mod_tests_block(line) {
            skip_depth = brace_delta(line).max(0);
            continue;
        }
        for marker in MARKERS {
            if line.contains(marker) {
                hits.push((line_no, (*marker).to_owned()));
            }
        }
        for _ in command_marker_hits(line) {
            hits.push((line_no, "Command::".to_owned()));
        }
    }
    hits
}

fn brace_delta(line: &str) -> i32 {
    let opens = i32::try_from(line.matches('{').count()).unwrap_or(i32::MAX);
    let closes = i32::try_from(line.matches('}').count()).unwrap_or(i32::MAX);
    opens - closes
}

const fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether comment/string-stripped `line` contains `\bmod\s+tests\s*\{`.
fn starts_mod_tests_block(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find("mod") {
        let idx = search_from + rel;
        let left_ok = idx == 0 || !is_ident_char(bytes[idx - 1] as char);
        let right_ok = bytes
            .get(idx + 3)
            .is_none_or(|&b| !is_ident_char(b as char));
        if left_ok && right_ok {
            let mut j = idx + 3;
            while bytes.get(j).is_some_and(|&b| (b as char).is_whitespace()) {
                j += 1;
            }
            if line[j..].starts_with("tests") {
                let after_tests = j + "tests".len();
                let word_ok = bytes
                    .get(after_tests)
                    .is_none_or(|&b| !is_ident_char(b as char));
                if word_ok {
                    let mut k = after_tests;
                    while bytes.get(k).is_some_and(|&b| (b as char).is_whitespace()) {
                        k += 1;
                    }
                    if bytes.get(k) == Some(&b'{') {
                        return true;
                    }
                }
            }
        }
        search_from = idx + 3;
    }
    false
}

/// Byte offsets of every bare, word-bounded `Command::` in comment/string-
/// stripped `line`, excluding `std::process::Command::` and the `::new(`
/// constructor that names (`phux_protocol`'s wire `Command` enum has no
/// associated `new` function; `std::process::Command::new` is the only
/// common source of this shape).
fn command_marker_hits(line: &str) -> Vec<usize> {
    const PAT: &str = "Command::";
    let mut hits = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(PAT) {
        let idx = search_from + rel;
        let bytes = line.as_bytes();
        let left_ok = idx == 0 || !is_ident_char(bytes[idx - 1] as char);
        let context_start = idx.saturating_sub(24);
        // `.floor_char_boundary`-free: `context_start` only needs to be a
        // valid slice start, and ASCII punctuation/identifier text around a
        // `Command::` occurrence never straddles a multi-byte boundary here.
        let looks_like_std_process = line
            .get(context_start..idx)
            .is_some_and(|ctx| ctx.contains("process::"));
        let after = &line[idx + PAT.len()..];
        let looks_like_ctor = after.trim_start().starts_with("new(");
        if left_ok && !looks_like_std_process && !looks_like_ctor {
            hits.push(idx);
        }
        search_from = idx + PAT.len();
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_blanks_comments_and_string_contents_but_keeps_layout() {
        let src = "let x = 1; // Command::Foo in a comment\n\
                    let s = \"FrameKind::Bar {ignored}\";\n\
                    /* block Connection::connect */\n\
                    real_code();\n";
        let stripped = strip_comments_and_strings(src);
        assert_eq!(stripped.lines().count(), src.lines().count());
        assert!(!stripped.contains("Command::Foo"));
        assert!(!stripped.contains("FrameKind::Bar"));
        assert!(!stripped.contains("Connection::connect"));
        assert!(stripped.contains("real_code();"));
    }

    #[test]
    fn format_string_braces_do_not_corrupt_brace_counting() {
        let src = "mod tests {\n    eprintln!(\"{a} {b} {c}\");\n}\nafter();\n";
        let hits = production_hits(&format!("{src}Connection::connect(x);\n"));
        // Only the trailing, out-of-test-module call should be visible.
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "Connection::connect");
    }

    #[test]
    fn command_marker_excludes_std_process_and_new() {
        assert!(command_marker_hits("std::process::Command::new(\"ls\")").is_empty());
        assert!(command_marker_hits("Command::new(\"ls\")").is_empty());
        assert_eq!(
            command_marker_hits("conn.request(1, Command::GetState { .. }).await?"),
            vec!["conn.request(1, ".len()]
        );
    }

    #[test]
    fn interspersed_test_modules_do_not_hide_production_code_between_them() {
        let src = "mod tests {\n  Connection::connect(a);\n}\n\
                    real_marker: Connection::connect(b);\n\
                    mod tests {\n  Connection::connect(c);\n}\n";
        let hits = production_hits(src);
        assert_eq!(
            hits.len(),
            1,
            "only the code between the two test modules should count"
        );
    }
}
