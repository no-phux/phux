//! `phux agent hook-transcript`: the Claude shim's transcript reader.
//!
//! Reads one Claude Code hook payload on stdin and prints at most one line:
//! the `data` object of a `provider_raw` record in the `phux.transcript/v1`
//! convention (ADR-0156), which the wrapper feeds to `phux agent emit --type
//! provider_raw --data -` on stdin. Prints nothing for any other event.
//!
//! - `UserPromptSubmit`: a `user` entry with the prompt text.
//! - `PostToolUse` and `PostToolUseFailure`: a `tool` entry keyed by
//!   `tool_use_id` with the tool name, a one-line summary of `tool_input`,
//!   and `ok` or `error`. Its `output` is empty unless
//!   `PHUX_AGENT_TRANSCRIPT=full`, which adds the tail of `tool_response`:
//!   tool output and file contents are not on the pane's screen.
//! - `Stop`: the turn's last assistant message, from `last_assistant_message`
//!   when Claude supplies it, else read from the tail of `transcript_path`.
//!
//! Payload text reaches a `phux` process only on stdin, never on a command
//! line. Hidden machine plumbing that never fails: an unreadable payload
//! prints nothing and exits 0, so a hook never breaks Claude.

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::process::ExitCode;

use phux_client::agent_transcript::{
    ToolStatus, TranscriptEntry, TranscriptRole, TranscriptTool, one_line,
};
use serde_json::Value;

/// The provider word every entry from this reader carries.
const PROVIDER: &str = "claude";

/// Largest payload read (a `PostToolUse` payload carries the whole
/// `tool_response`); a larger one prints nothing.
const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// How much of the end of a Claude transcript is scanned for the last
/// assistant message.
const TRANSCRIPT_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// `tool_input` keys that name what a call acts on, most telling first.
const SUMMARY_KEYS: &[&str] = &[
    "command",
    "file_path",
    "notebook_path",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
];

/// `tool_input` keys that carry file contents or edit bodies; never summarized.
const CONTENT_KEYS: &[&str] = &[
    "content",
    "contents",
    "new_string",
    "old_string",
    "newText",
    "oldText",
    "new_source",
    "edits",
    "text",
    "body",
    "data",
    "patch",
    "diff",
];

/// Longest value shown per argument in a fallback summary, in characters.
const SUMMARY_VALUE_CHARS: usize = 80;

pub(super) fn run() -> ExitCode {
    let mut raw = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(MAX_PAYLOAD_BYTES + 1)
        .read_to_end(&mut raw);
    if read.is_err() || raw.len() as u64 > MAX_PAYLOAD_BYTES {
        return ExitCode::SUCCESS;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let full = std::env::var("PHUX_AGENT_TRANSCRIPT").is_ok_and(|mode| mode == "full");
    if let Some(data) = render(&raw, now_ms, full, read_last_assistant) {
        let mut out = std::io::stdout().lock();
        // A closed stdout (the wrapper gave up) is not this helper's problem.
        let _ = writeln!(out, "{data}");
        let _ = out.flush();
    }
    ExitCode::SUCCESS
}

/// The record `data` for one raw hook payload, or `None` when the event has
/// no transcript entry. `full` adds tool output. `transcript` reads `(message id, text)` of the last
/// assistant message from a transcript path; injected for tests.
fn render(
    raw: &[u8],
    now_ms: u128,
    full: bool,
    transcript: impl Fn(&Path) -> Option<(String, String)>,
) -> Option<Value> {
    let payload: Value = serde_json::from_slice(raw).ok()?;
    let entry = match payload.get("hook_event_name")?.as_str()? {
        "UserPromptSubmit" => prompt_entry(&payload, now_ms)?,
        "PostToolUse" => tool_entry(&payload, false, full)?,
        "PostToolUseFailure" => tool_entry(&payload, true, full)?,
        "Stop" => assistant_entry(&payload, now_ms, transcript)?,
        _ => return None,
    };
    Some(entry.into_data(PROVIDER))
}

fn text_entry(id: String, role: TranscriptRole, text: &str) -> TranscriptEntry {
    TranscriptEntry {
        id,
        role,
        text: text.to_owned(),
        truncated: false,
        is_final: true,
        tool: None,
    }
}

fn prompt_entry(payload: &Value, now_ms: u128) -> Option<TranscriptEntry> {
    let prompt = payload.get("prompt")?.as_str()?;
    if prompt.trim().is_empty() {
        return None;
    }
    Some(text_entry(
        format!("user-{now_ms}"),
        TranscriptRole::User,
        prompt,
    ))
}

fn tool_entry(payload: &Value, failed: bool, full: bool) -> Option<TranscriptEntry> {
    let name = payload.get("tool_name")?.as_str()?;
    let call_id = payload
        .get("tool_use_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let response = payload.get("tool_response").unwrap_or(&Value::Null);
    let status = if failed || response_failed(response) {
        ToolStatus::Error
    } else {
        ToolStatus::Ok
    };
    // Tool output and file contents are not on the pane's screen; only
    // `PHUX_AGENT_TRANSCRIPT=full` carries them (ADR-0156).
    let output = match (full, failed) {
        (false, _) => String::new(),
        (true, true) => payload.get("error").map(value_text).unwrap_or_default(),
        (true, false) => value_text(response),
    };
    Some(TranscriptEntry {
        tool: Some(TranscriptTool {
            name: name.to_owned(),
            call_id: call_id.to_owned(),
            summary: summarize_input(payload.get("tool_input").unwrap_or(&Value::Null)),
            status,
            output,
        }),
        ..text_entry(call_id.to_owned(), TranscriptRole::Tool, "")
    })
}

fn assistant_entry(
    payload: &Value,
    now_ms: u128,
    transcript: impl Fn(&Path) -> Option<(String, String)>,
) -> Option<TranscriptEntry> {
    let supplied = payload
        .get("last_assistant_message")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty());
    if let Some(text) = supplied {
        return Some(text_entry(
            format!("assistant-{now_ms}"),
            TranscriptRole::Assistant,
            text,
        ));
    }
    let path = payload.get("transcript_path")?.as_str()?;
    let (id, text) = transcript(Path::new(path))?;
    Some(text_entry(
        format!("assistant-{id}"),
        TranscriptRole::Assistant,
        &text,
    ))
}

/// A one-line summary of a tool's arguments: the first naming argument (a
/// command, path, pattern, URL, query) when present, else `key=value` for its
/// scalar arguments, values cut short and [`CONTENT_KEYS`] left out. Never the
/// arguments' bodies.
fn summarize_input(input: &Value) -> String {
    let summary = match input {
        Value::String(text) => text.clone(),
        Value::Object(object) => {
            let named = SUMMARY_KEYS
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str))
                .filter(|text| !text.trim().is_empty());
            named.map_or_else(
                || {
                    object
                        .iter()
                        .filter(|(key, _)| !CONTENT_KEYS.contains(&key.as_str()))
                        .map(|(key, value)| format!("{key}={}", scalar_summary(value)))
                        .collect::<Vec<_>>()
                        .join(" ")
                },
                str::to_owned,
            )
        }
        _ => String::new(),
    };
    one_line(
        &summary,
        phux_client::agent_transcript::MAX_TOOL_SUMMARY_CHARS,
    )
}

fn scalar_summary(value: &Value) -> String {
    match value {
        Value::String(text) => one_line(text, SUMMARY_VALUE_CHARS),
        Value::Number(_) | Value::Bool(_) | Value::Null => value.to_string(),
        Value::Array(_) | Value::Object(_) => "\u{2026}".to_owned(),
    }
}

fn response_failed(response: &Value) -> bool {
    response.get("is_error").and_then(Value::as_bool) == Some(true)
        || response
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| !error.is_empty())
}

/// The readable text of a tool response or error: a string as itself, an
/// array as its blocks' text, `stdout`/`stderr`, a `content` or `output`
/// field, a read file's content, and otherwise compact JSON.
fn value_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(value_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(object) => {
            let stream = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or("");
            let streams = [stream("stdout"), stream("stderr")]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !streams.is_empty() {
                return streams;
            }
            for key in ["text", "content", "output", "result", "error"] {
                if let Some(inner) = object.get(key) {
                    let text = value_text(inner);
                    if !text.is_empty() {
                        return text;
                    }
                }
            }
            if let Some(file) = object.get("file") {
                let text = value_text(file);
                if !text.is_empty() {
                    return text;
                }
            }
            value.to_string()
        }
        Value::Bool(_) | Value::Number(_) => value.to_string(),
    }
}

/// `(message id, text)` of the last assistant message with text in the tail
/// of a Claude transcript (one JSON object per line). Claude writes one line
/// per content block, so the text blocks sharing the last message id are
/// joined in order.
fn read_last_assistant(path: &Path) -> Option<(String, String)> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(TRANSCRIPT_TAIL_BYTES)
        .read_to_end(&mut tail)
        .ok()?;
    let text = String::from_utf8_lossy(&tail);
    // A seek into the middle of the file lands mid-line; drop that fragment.
    let whole = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    last_assistant(whole)
}

fn last_assistant(jsonl: &str) -> Option<(String, String)> {
    let mut id: Option<String> = None;
    let mut blocks: Vec<String> = Vec::new();
    for line in jsonl.lines().rev() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match record.get("type").and_then(Value::as_str) {
            Some("assistant") => {}
            // A user line (a prompt or a tool result) ends the message.
            Some("user") if id.is_some() => break,
            _ => continue,
        }
        let Some(message) = record.get("message") else {
            continue;
        };
        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| record.get("uuid").and_then(Value::as_str))
            .unwrap_or_default();
        let text = message_text(message);
        match &id {
            None if text.is_empty() => {}
            None => {
                id = Some(message_id.to_owned());
                blocks.push(text);
            }
            Some(current) if current == message_id => {
                if !text.is_empty() {
                    blocks.push(text);
                }
            }
            Some(_) => break,
        }
    }
    blocks.reverse();
    Some((id?, blocks.join("\n")))
}

fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::{last_assistant, read_last_assistant, render};
    use phux_client::agent_transcript::{
        MAX_TOOL_OUTPUT_BYTES, MAX_TOOL_SUMMARY_CHARS, MAX_TRANSCRIPT_DATA_BYTES,
    };
    use serde_json::{Value, json};
    use std::path::Path;

    fn no_transcript(_: &Path) -> Option<(String, String)> {
        None
    }

    fn render_json(payload: &Value) -> Option<Value> {
        render(payload.to_string().as_bytes(), 1_700, false, no_transcript)
    }

    /// `PHUX_AGENT_TRANSCRIPT=full`: tool entries carry their output.
    fn render_full(payload: &Value) -> Option<Value> {
        render(payload.to_string().as_bytes(), 1_700, true, no_transcript)
    }

    #[test]
    fn user_prompt_submit_becomes_a_final_user_entry() {
        let data = render_json(&json!({
            "session_id": "s1",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "fix the \u{1b}[31mbuild\u{1b}[0m please"
        }))
        .expect("an entry");
        assert_eq!(
            data,
            json!({
                "provider": "claude",
                "schema": "phux.transcript/v1",
                "entry": {
                    "id": "user-1700",
                    "role": "user",
                    "text": "fix the build please",
                    "truncated": false,
                    "final": true
                }
            })
        );
    }

    /// The default carries what the pane shows: the tool, a one-line summary,
    /// and its status. Output and file contents need `full`.
    #[test]
    fn by_default_a_tool_entry_carries_no_output() {
        let read = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_use_id": "t1",
            "tool_input": { "file_path": "/repo/.env" },
            "tool_response": { "type": "text", "file": { "filePath": "/repo/.env", "content": "API_KEY=SECRET" } }
        });
        let data = render_json(&read).expect("an entry");
        assert_eq!(
            data["entry"]["tool"],
            json!({
                "name": "Read",
                "call_id": "t1",
                "summary": "/repo/.env",
                "status": "ok",
                "output": ""
            })
        );
        assert!(!data.to_string().contains("SECRET"));
        let full = render_full(&read).expect("an entry");
        assert_eq!(full["entry"]["tool"]["output"], "API_KEY=SECRET");

        let failed = render_json(&json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Bash",
            "tool_use_id": "t2",
            "tool_input": { "command": "false" },
            "error": "SECRET exit 1"
        }))
        .expect("an entry");
        assert_eq!(failed["entry"]["tool"]["status"], "error");
        assert_eq!(failed["entry"]["tool"]["output"], "");
    }

    /// Summaries name what a call acts on and never carry a body.
    #[test]
    fn summaries_leave_out_file_contents_and_edit_bodies() {
        let summary = |input: Value| super::summarize_input(&input);
        assert_eq!(
            summary(json!({ "file_path": "a.rs", "content": "SECRET" })),
            "a.rs"
        );
        assert_eq!(
            summary(json!({ "notebook_path": "n.ipynb", "new_source": "SECRET" })),
            "n.ipynb"
        );
        assert_eq!(
            summary(json!({
                "server": "x",
                "content": "SECRET",
                "body": "SECRET",
                "nested": { "a": 1 },
                "long": "y".repeat(200)
            })),
            format!("long={} nested=\u{2026} server=x", "y".repeat(80))
        );
        assert_eq!(summary(json!(["SECRET"])), "");
        assert_eq!(summary(Value::Null), "");
    }

    #[test]
    fn post_tool_use_becomes_a_tool_entry_keyed_by_the_call() {
        let data = render_full(&json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "toolu_01",
            "tool_input": { "command": "cargo test\n  -p phux", "description": "Run tests" },
            "tool_response": { "stdout": "ok\n", "stderr": "warning: x", "interrupted": false }
        }))
        .expect("an entry");
        assert_eq!(
            data["entry"],
            json!({
                "id": "toolu_01",
                "role": "tool",
                "text": "",
                "truncated": false,
                "final": true,
                "tool": {
                    "name": "Bash",
                    "call_id": "toolu_01",
                    "summary": "cargo test -p phux",
                    "status": "ok",
                    "output": "ok\n\nwarning: x"
                }
            })
        );
    }

    #[test]
    fn tool_errors_and_shapes_are_read() {
        let failed = render_full(&json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "mcp__phux__phux_ls",
            "tool_use_id": "t2",
            "tool_input": { "all": true },
            "tool_response": [{ "type": "text", "text": "boom" }, { "is_error": true }]
        }))
        .expect("an entry");
        let tool = &failed["entry"]["tool"];
        assert_eq!(tool["summary"], "all=true");
        assert_eq!(tool["output"], "boom\n{\"is_error\":true}");
        assert_eq!(
            tool["status"], "ok",
            "only an object-level flag is an error"
        );

        let flagged = render_full(&json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_use_id": "t3",
            "tool_input": { "file_path": "/repo/src/lib.rs", "old_string": "a" },
            "tool_response": { "is_error": true, "content": "String not found" }
        }))
        .expect("an entry");
        assert_eq!(flagged["entry"]["tool"]["status"], "error");
        assert_eq!(flagged["entry"]["tool"]["summary"], "/repo/src/lib.rs");
        assert_eq!(flagged["entry"]["tool"]["output"], "String not found");

        let failure = render_full(&json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Bash",
            "tool_use_id": "t4",
            "tool_input": { "command": "false" },
            "error": "exit 1"
        }))
        .expect("an entry");
        assert_eq!(failure["entry"]["tool"]["status"], "error");
        assert_eq!(failure["entry"]["tool"]["output"], "exit 1");
    }

    #[test]
    fn tool_fields_stay_within_their_caps() {
        let data = render_full(&json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "t5",
            "tool_input": { "command": "x".repeat(5_000) },
            "tool_response": { "stdout": format!("{}END", "y".repeat(100_000)) }
        }))
        .expect("an entry");
        let tool = &data["entry"]["tool"];
        assert_eq!(
            tool["summary"].as_str().unwrap().chars().count(),
            MAX_TOOL_SUMMARY_CHARS
        );
        let output = tool["output"].as_str().unwrap();
        assert_eq!(output.len(), MAX_TOOL_OUTPUT_BYTES);
        assert!(output.ends_with("END"));
    }

    #[test]
    fn stop_prefers_the_supplied_message_then_the_transcript() {
        let supplied = render_json(&json!({
            "hook_event_name": "Stop",
            "transcript_path": "/nonexistent",
            "last_assistant_message": "All green."
        }))
        .expect("an entry");
        assert_eq!(supplied["entry"]["id"], "assistant-1700");
        assert_eq!(supplied["entry"]["text"], "All green.");

        let payload = json!({ "hook_event_name": "Stop", "transcript_path": "/t.jsonl" });
        let read = render(payload.to_string().as_bytes(), 1, false, |path| {
            assert_eq!(path, Path::new("/t.jsonl"));
            Some(("msg_9".to_owned(), "Done.".to_owned()))
        })
        .expect("an entry");
        assert_eq!(read["entry"]["id"], "assistant-msg_9");
        assert_eq!(read["entry"]["role"], "assistant");
        assert_eq!(read["entry"]["final"], true);

        assert_eq!(
            render_json(&payload),
            None,
            "an unreadable transcript emits nothing"
        );
    }

    #[test]
    fn a_long_assistant_message_is_cut_to_fit_the_record() {
        let data = render_json(&json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "\"quoted\" ".repeat(10_000)
        }))
        .expect("an entry");
        assert!(data.to_string().len() <= MAX_TRANSCRIPT_DATA_BYTES);
        assert_eq!(data["entry"]["truncated"], true);
    }

    #[test]
    fn other_events_and_bad_payloads_print_nothing() {
        for event in ["SessionStart", "PreToolUse", "Notification", "SessionEnd"] {
            assert_eq!(
                render_json(&json!({ "hook_event_name": event, "prompt": "x" })),
                None
            );
        }
        assert_eq!(render(b"", 0, false, no_transcript), None);
        assert_eq!(render(b"not json", 0, false, no_transcript), None);
        assert_eq!(
            render_json(&json!({ "hook_event_name": "UserPromptSubmit", "prompt": "  " })),
            None
        );
        assert_eq!(
            render_json(&json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" })),
            None,
            "a tool entry needs its call id"
        );
    }

    #[test]
    fn the_last_assistant_message_joins_its_text_blocks() {
        let lines = [
            json!({"type":"user","message":{"role":"user","content":"go"}}),
            json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Earlier."}]}}),
            json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}),
            json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
            json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"thinking","thinking":"hmm"}]}}),
            json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"text","text":"First."}]}}),
            json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"text","text":"Second."}]}}),
            json!({"type":"system","subtype":"stop_hook_summary"}),
        ];
        let jsonl = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            last_assistant(&format!("{jsonl}\n{{truncated")),
            Some(("msg_2".to_owned(), "First.\nSecond.".to_owned()))
        );
        assert_eq!(last_assistant("{\"type\":\"user\"}\n"), None);

        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, &jsonl).unwrap();
        assert_eq!(
            read_last_assistant(&path).map(|(id, _)| id),
            Some("msg_2".to_owned())
        );
        assert_eq!(read_last_assistant(&dir.path().join("missing")), None);
    }
}
