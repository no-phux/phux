//! Grep gate (PHA-406 L21): CLI verbs that hand-roll wire frames belong in a
//! `phux-client` module (`Connection::connect` -> `request` -> typed result
//! -> typed error, the `send_keys.rs` shape), not in `crates/phux/src/commands/`.
//!
//! Every `.rs` file under `src/commands/` fails if a raw-wire marker appears
//! in its production code (comments, string literals, and `mod tests` bodies
//! excluded) beyond its per-file [`ALLOWLIST`] cap.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test-only file-scanning gate: a read failure here is a test-harness bug, \
              not a condition to recover from"
)]

#[path = "common/runner.rs"]
mod runner;

use std::path::{Path, PathBuf};

/// Files (relative to `src/commands/`) still allowed raw wire work, as
/// `(path, max_hits, why)`. `max_hits` is a ceiling: lower it freely, raise
/// it only with a reviewed reason.
const ALLOWLIST: &[(&str, usize, &str)] = &[
    (
        "mod.rs",
        28,
        "the shared `command_on`/`request_command` helpers, plus matches on the CLI's own `Command` enum",
    ),
    (
        "server_target.rs",
        2,
        "the one real connect a `ServerSpec` resolves to",
    ),
    (
        "spawn.rs",
        3,
        "request-shaping for the placed spawn, not round trips",
    ),
    ("launch.rs", 1, "builds its `SpawnResource` request literal"),
    ("play.rs", 1, "`phux play`'s own `SPAWN_RESOURCE`"),
    ("bootstrap.rs", 3, "`phux bootstrap`'s `OPEN_LISTENER`"),
    ("config.rs", 4, "`config set`'s `SET_METADATA`"),
    ("perf.rs", 1, "`GET_PERF`"),
    ("status.rs", 1, "`GET_STATE` for `phux status`"),
    ("enroll.rs", 1, "pairing/enrollment dials"),
    ("whoami.rs", 1, "`GET_METADATA` of `phux.whoami/v1`"),
    (
        "workspace/archive.rs",
        3,
        "`workspace restore`'s session create",
    ),
    ("agent/answer.rs", 1, "`phux agent answer`"),
    ("agent/report_state.rs", 2, "`REPORT_AGENT_STATE`"),
    (
        "agent/resource_session.rs",
        1,
        "`phux agent session` internals",
    ),
    ("agent/start.rs", 8, "`phux agent start`"),
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
    let commands_dir = crate::runner::manifest_dir().join("src/commands");
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

/// Blank out comments and string-literal contents character-for-character,
/// keeping line numbers and brace positions. Biased toward under-stripping
/// (char literals and nested block comments are not special-cased), which
/// can only surface more code, never hide it.
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
