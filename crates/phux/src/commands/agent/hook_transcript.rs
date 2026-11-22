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
                    // ACP enables serde_json/preserve_order for the workspace.
                    // Keep this summary independent of provider key order.
                    let mut fields: Vec<_> = object.iter().collect();
                    fields.sort_unstable_by_key(|(key, _)| *key);
                    fields
                        .into_iter()
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
mod tests;
