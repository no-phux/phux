//! Unit tests for the command lines the wrapper's hook arms emit.

use super::tests::{Harness, rendered};

/// Each arm's exact `phux` command lines: the session is opened once
/// with Claude's own session id, records carry only counts, tool names,
/// and kinds, `blocked` still feeds the attention ladder, and `clear`
/// ends and closes the session. Pinned as argv so a change here is a change to
/// the producer contract, visibly.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one table of arms, each pinned as argv"
)]
fn stream_arms_emit_exactly_these_command_lines() {
    let h = Harness::new();
    let fields = |event: &str, tool: &str, kind: &str, chars: &str, reason: &str, source: &str| {
        format!("sess-1 {event} {tool} {kind} {chars} {reason} {source}")
    };
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "start",
            fields("SessionStart", "-", "-", "0", "-", "startup"),
            vec![
                "agent hook-payload",
                "agent set @42 --name claude --kind claude",
                "agent session open @42 --provider claude --native-id=sess-1",
                "agent emit @42 --type session_start",
            ],
        ),
        (
            "working",
            fields("UserPromptSubmit", "-", "-", "18", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type prompt --data {\"chars\":18}",
                "agent hook-transcript",
            ],
        ),
        (
            "tool-start",
            fields("PreToolUse", "Bash", "-", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type tool_start --data {\"tool_name\":\"Bash\"}",
            ],
        ),
        (
            "tool-end",
            fields("PostToolUse", "mcp__phux__phux_ls", "-", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type tool_end --data {\"tool_name\":\"mcp__phux__phux_ls\"}",
                "agent hook-transcript",
            ],
        ),
        (
            "tool-end",
            fields("PostToolUseFailure", "Bash", "-", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type tool_end --data {\"tool_name\":\"Bash\",\"ok\":false}",
                "agent hook-transcript",
            ],
        ),
        (
            "blocked",
            fields("PermissionRequest", "Bash", "-", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type ask",
                "ask @42 Claude needs attention",
            ],
        ),
        (
            "blocked",
            fields("Notification", "-", "permission_prompt", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type notification --data {\"kind\":\"permission\"}",
                "ask @42 Claude needs attention",
            ],
        ),
        (
            "blocked",
            fields("Notification", "-", "elicitation_dialog", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type notification --data {\"kind\":\"elicitation\"}",
                "ask @42 Claude needs attention",
            ],
        ),
        (
            "blocked",
            fields("Notification", "-", "idle_prompt", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type notification --data {\"kind\":\"idle\"}",
                "ask @42 Claude needs attention",
            ],
        ),
        (
            "done",
            fields("Stop", "-", "-", "0", "-", "-"),
            vec![
                "agent hook-payload",
                "agent hook-transcript",
                "agent emit @42 --type stop",
            ],
        ),
        (
            "clear",
            fields("SessionEnd", "-", "-", "0", "prompt_input_exit", "-"),
            vec![
                "agent hook-payload",
                "agent emit @42 --type session_end --data {\"reason\":\"prompt_input_exit\"}",
                "agent session close @42",
                "agent clear @42",
            ],
        ),
    ];
    for (arm, fields, want) in cases {
        let log = h.hook(arm, "{}", &fields, false);
        assert_eq!(log, want, "arm `{arm}` with fields `{fields}`");
    }

    // With no session id (a payload-less start, or a helper that could
    // not parse), `start` writes identity and opens nothing: the native
    // id is the join key, and a second open would create a second
    // session.
    let log = h.hook("start", "", "- - - - 0 - -", false);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent set @42 --name claude --kind claude",
        ]
    );
    // A `compact`-sourced SessionStart arrives with no SessionEnd before
    // it, so opening here would leave two live sessions under the pane.
    let log = h.hook("start", "", "sess-1 SessionStart - - 0 - compact", false);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent set @42 --name claude --kind claude",
        ]
    );
    // `clear` still ends, closes, and clears without a reason.
    let log = h.hook("clear", "", "- - - - 0 - -", false);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent emit @42 --type session_end",
            "agent session close @42",
            "agent clear @42",
        ]
    );
}

/// No byte of the hook payload reaches a `phux` command line except
/// through the helper's six tokens: the prompt text, `tool_input`, and
/// `tool_response` markers never appear in any argv, and the whole
/// payload is forwarded only under `PHUX_AGENT_EMIT_RAW=1`, only on stdin
/// of a `provider_raw` emit. (Transcript entries also travel on
/// stdin only; `transcript_entries_ride_provider_raw_on_stdin_by_default`.)
#[test]
fn payload_text_never_reaches_a_command_line_unless_raw_is_opted_in() {
    let wrapper = rendered();
    for forbidden in [
        "tool_input",
        "tool_response",
        "transcript_path",
        "$prompt",
        "\"$payload\" |",
    ] {
        assert!(
            !wrapper.contains(forbidden),
            "the wrapper text must not reference `{forbidden}`:\n{wrapper}"
        );
    }

    let h = Harness::new();
    let payload = r#"{"session_id":"sess-1","hook_event_name":"PreToolUse","tool_name":"Bash","prompt":"PROMPT-MARKER","tool_input":{"command":"INPUT-MARKER"}}"#;
    for arm in [
        "start",
        "working",
        "tool-start",
        "tool-end",
        "blocked",
        "done",
        "clear",
    ] {
        let log = h.hook(arm, payload, "sess-1 PreToolUse Bash - 13 - -", false);
        assert!(
            log.iter().all(|line| !line.contains("MARKER")),
            "arm `{arm}` leaked payload text: {log:?}"
        );
    }

    let log = h.hook(
        "tool-start",
        payload,
        "sess-1 PreToolUse Bash - 13 - -",
        true,
    );
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent emit @42 --type tool_start --data {\"tool_name\":\"Bash\"}",
            "agent emit @42 --type provider_raw --data -",
            &format!("stdin:{payload}"),
        ],
        "raw opt-in forwards the payload once, on stdin, after the typed record"
    );
    // `session_end` is terminal, so on `clear` the raw record goes first.
    let log = h.hook("clear", payload, "sess-1 SessionEnd - - 0 other -", true);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent emit @42 --type provider_raw --data -",
            &format!("stdin:{payload}"),
            "agent emit @42 --type session_end --data {\"reason\":\"other\"}",
            "agent session close @42",
            "agent clear @42",
        ],
        "nothing may be emitted after session_end"
    );
}

/// Transcript entries are on by default (ADR-0156): the `working`,
/// `tool-end`, and `done` arms feed what `agent hook-transcript` prints to
/// a `provider_raw` emit on stdin, `done` before its terminal-for-the-turn
/// `stop`. `PHUX_AGENT_TRANSCRIPT=0` turns them off, an empty helper
/// answer emits nothing.
#[test]
fn transcript_entries_ride_provider_raw_on_stdin_by_default() {
    let h = Harness::new();
    let entry = r#"{"provider":"claude","schema":"phux.transcript/v1","entry":{"id":"user-1","role":"user","text":"hi","truncated":false,"final":true}}"#;
    let on = [("FAKE_TRANSCRIPT", entry)];
    let fields = "sess-1 UserPromptSubmit - - 2 - -";
    let log = h.hook_env("working", "{}", fields, false, &on);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent emit @42 --type prompt --data {\"chars\":2}",
            "agent hook-transcript",
            "agent emit @42 --type provider_raw --data -",
            &format!("stdin:{entry}"),
        ]
    );
    let log = h.hook_env("done", "{}", "sess-1 Stop - - 0 - -", false, &on);
    assert_eq!(
        log,
        [
            "agent hook-payload",
            "agent hook-transcript",
            "agent emit @42 --type provider_raw --data -",
            &format!("stdin:{entry}"),
            "agent emit @42 --type stop",
        ]
    );
    for arm in ["start", "tool-start", "blocked", "clear"] {
        let log = h.hook_env(arm, "{}", fields, false, &on);
        assert!(
            !log.iter().any(|line| line.contains("hook-transcript")),
            "arm `{arm}` has no transcript entry: {log:?}"
        );
    }
    let off = [("FAKE_TRANSCRIPT", entry), ("PHUX_AGENT_TRANSCRIPT", "0")];
    for arm in ["working", "tool-end", "done"] {
        let log = h.hook_env(arm, "{}", fields, false, &off);
        assert!(
            !log.iter()
                .any(|line| line.contains("transcript") || line.contains("provider_raw")),
            "opted out, arm `{arm}` must not read or emit a transcript: {log:?}"
        );
    }
    let silent = h.hook_env("tool-end", "{}", fields, false, &[]);
    assert!(
        !silent.iter().any(|line| line.contains("provider_raw")),
        "an empty helper answer emits nothing: {silent:?}"
    );

    // The payload copy and the entry file are removed when the hook exits.
    let tmp = tempfile::tempdir().expect("scratch TMPDIR");
    let tmpdir = tmp.path().to_str().expect("utf-8 path");
    for arm in ["working", "tool-end", "done", "start"] {
        let log = h.hook_env(
            arm,
            "{}",
            fields,
            false,
            &[("FAKE_TRANSCRIPT", entry), ("TMPDIR", tmpdir)],
        );
        assert!(!log.is_empty());
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("read TMPDIR")
            .collect();
        assert!(left.is_empty(), "arm `{arm}` left temp files: {left:?}");
    }
}
