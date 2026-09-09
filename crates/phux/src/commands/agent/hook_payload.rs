//! `phux agent hook-payload` — the Claude shim's stdin reader.
//!
//! Claude Code hands every hook its event as one JSON object on stdin. The
//! generated `/bin/sh` wrapper (`shim.rs`) needs a handful of scalar fields
//! out of it and has no JSON parser, so it pipes the object here and reads
//! back ONE line of whitespace-separated, shell-safe tokens in a fixed order:
//!
//! ```text
//! <session_id> <hook_event_name> <tool_name> <notification_type> <prompt_chars> <reason> <source>
//! ```
//!
//! `reason` is `SessionEnd`'s; `source` is `SessionStart`'s (`startup`,
//! `resume`, `clear`, `compact`, `fork`) — the wrapper refuses to open a
//! second session on a `compact`, which Claude fires without a `SessionEnd`.
//!
//! Every field the wrapper could splice into a command line is reduced to the
//! charset `[A-Za-z0-9._:@/+-]` (anything else becomes `_`), capped at
//! [`MAX_FIELD_CHARS`], and printed as `-` when absent — so `set -- $line`
//! in the wrapper can never split, glob, or quote-break, and a value can be
//! passed straight into `--data '{"tool_name":"<value>"}'` without escaping.
//!
//! What is deliberately NOT printed: the prompt text (only its character
//! count), `tool_input`, `tool_response`, `last_assistant_message`,
//! `transcript_path`. The wrapper forwards the whole payload only when the
//! user opts in with `PHUX_AGENT_EMIT_RAW=1`, and it does that by re-reading
//! its own copy of stdin, never through this helper.
//!
//! Hidden like `phux play --pty-writer`: machine plumbing for the wrapper, not
//! a promise that phux ships a JSON tool. Never fails: an empty, oversized, or
//! malformed payload prints the all-absent line and exits 0, because a hook
//! that breaks Claude to report a parse error has its priorities inverted.

use std::io::{Read as _, Write as _};
use std::process::ExitCode;

/// Longest token printed for any string field. Claude session ids are UUIDs
/// (36 chars); MCP tool names are `mcp__<server>__<tool>` and fit comfortably.
const MAX_FIELD_CHARS: usize = 128;

/// Largest payload read before giving up and printing the absent line. A
/// `PostToolUse` payload carries the whole `tool_response`, which can run to
/// megabytes; the fields this helper wants are all near the top, but
/// `serde_json` needs the whole document, so the ceiling is generous.
const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// Placeholder for an absent field.
const ABSENT: &str = "-";

pub(super) fn run() -> ExitCode {
    let mut raw = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_end(&mut raw);
    let line = match read {
        Ok(_) => render_line(&raw),
        Err(_) => absent_line(),
    };
    let mut out = std::io::stdout().lock();
    // A closed stdout (the wrapper gave up) is not this helper's problem.
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    ExitCode::SUCCESS
}

/// The wrapper-facing line for one raw payload. Pure, so the field extraction
/// and the sanitizer are pinned without a process.
fn render_line(raw: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return absent_line();
    };
    let field = |key: &str| sanitize(value.get(key).and_then(serde_json::Value::as_str));
    let prompt_chars = value
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .map_or(0, |prompt| prompt.chars().count());
    [
        field("session_id"),
        field("hook_event_name"),
        field("tool_name"),
        field("notification_type"),
        prompt_chars.to_string(),
        field("reason"),
        field("source"),
    ]
    .join(" ")
}

fn absent_line() -> String {
    [ABSENT, ABSENT, ABSENT, ABSENT, "0", ABSENT, ABSENT].join(" ")
}

/// Reduce a payload string to one shell-safe, JSON-safe token, or `-`.
fn sanitize(value: Option<&str>) -> String {
    let Some(value) = value else {
        return ABSENT.to_owned();
    };
    let token: String = value
        .chars()
        .take(MAX_FIELD_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '/' | '+' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if token.is_empty() {
        ABSENT.to_owned()
    } else {
        token
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_FIELD_CHARS, absent_line, render_line, sanitize};

    /// The seven fields, in order, from a realistic `PreToolUse` payload —
    /// and the payload's `tool_input` never reaches the line.
    #[test]
    fn extracts_the_wrapper_fields_in_fixed_order() {
        let payload = serde_json::json!({
            "session_id": "0d6f1c2e-4b1a-4e9f-9c1d-2a3b4c5d6e7f",
            "hook_event_name": "PreToolUse",
            "transcript_path": "/home/u/.claude/projects/x/transcript.jsonl",
            "cwd": "/home/u/project",
            "tool_name": "Bash",
            "tool_input": { "command": "rm -rf /tmp/SECRET-MARKER" },
            "tool_use_id": "toolu_01"
        });
        let line = render_line(payload.to_string().as_bytes());
        assert_eq!(
            line,
            "0d6f1c2e-4b1a-4e9f-9c1d-2a3b4c5d6e7f PreToolUse Bash - 0 - -"
        );
        assert!(!line.contains("SECRET"));
        assert!(!line.contains("transcript"));
    }

    /// The prompt is reported as a character count only — never its text —
    /// and the count is in characters, not bytes.
    #[test]
    fn prompt_is_reduced_to_a_character_count() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "héllo SECRET-MARKER"
        });
        let line = render_line(payload.to_string().as_bytes());
        assert_eq!(line, "s1 UserPromptSubmit - - 19 - -");
        assert!(!line.contains("SECRET"));
    }

    /// `Notification` carries its kind; `SessionEnd` its reason;
    /// `SessionStart` its source.
    #[test]
    fn notification_type_reason_and_source_ride_their_own_slots() {
        let notification = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude wants to run: npm install"
        });
        assert_eq!(
            render_line(notification.to_string().as_bytes()),
            "s1 Notification - permission_prompt 0 - -"
        );
        let end = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "SessionEnd",
            "reason": "prompt_input_exit"
        });
        assert_eq!(
            render_line(end.to_string().as_bytes()),
            "s1 SessionEnd - - 0 prompt_input_exit -"
        );
        let start = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "SessionStart",
            "source": "compact"
        });
        assert_eq!(
            render_line(start.to_string().as_bytes()),
            "s1 SessionStart - - 0 - compact"
        );
    }

    /// Malformed, empty, and non-object payloads all print the absent line
    /// rather than failing: a hook must never break Claude.
    #[test]
    fn unparseable_payloads_print_the_absent_line() {
        assert_eq!(render_line(b""), absent_line());
        assert_eq!(render_line(b"not json"), absent_line());
        assert_eq!(render_line(b"[1,2,3]"), absent_line());
        assert_eq!(render_line(b"{\"session_id\": 42}"), absent_line());
        assert_eq!(absent_line(), "- - - - 0 - -");
    }

    /// Anything that could split a word, glob, or break out of a quote is
    /// flattened to `_`; the output is safe for `set --` and for splicing
    /// into a JSON string literal.
    #[test]
    fn sanitizer_leaves_only_shell_and_json_safe_tokens() {
        assert_eq!(sanitize(Some("mcp__phux__phux_ls")), "mcp__phux__phux_ls");
        assert_eq!(
            sanitize(Some("a b\"c'd$e`f\\g*h?i[j]k\nl")),
            "a_b_c_d_e_f_g_h_i_j_k_l"
        );
        assert_eq!(sanitize(Some("")), "-");
        assert_eq!(sanitize(None), "-");
        let long = "x".repeat(MAX_FIELD_CHARS + 50);
        assert_eq!(sanitize(Some(&long)).len(), MAX_FIELD_CHARS);
        let tricky = sanitize(Some("é→ü"));
        assert!(tricky.is_ascii(), "{tricky}");
    }
}
