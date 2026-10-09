//! The `phux.transcript/v1` payload convention (ADR-0156).
//!
//! A first-party producer carries one conversation entry per `provider_raw`
//! record, as `data = {"provider", "schema", "entry"}`. The codec, the frame,
//! and the record type are unchanged: this module only names the shape inside
//! `data`, bounds it so the whole retained record fits [`MAX_RECORD_BYTES`],
//! and recognizes it on the way back out.
//!
//! An entry with a repeated `id` replaces the earlier one; `final: false`
//! marks a streaming partial that a later entry with the same `id` replaces.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::agent_stream::MAX_RECORD_BYTES;

/// The `data.schema` word that marks a transcript entry.
pub const TRANSCRIPT_SCHEMA: &str = "phux.transcript/v1";

/// Bytes kept free for the server's envelope (`seq`, `ts_ms`, `type`, the
/// `data` key, and the line terminator) around the producer's `data`.
pub const ENVELOPE_RESERVE_BYTES: usize = 256;

/// Ceiling on the serialized `data` object of one transcript record.
pub const MAX_TRANSCRIPT_DATA_BYTES: usize = MAX_RECORD_BYTES - ENVELOPE_RESERVE_BYTES;

/// Ceiling on `entry.tool.summary`, in characters.
pub const MAX_TOOL_SUMMARY_CHARS: usize = 512;

/// Ceiling on `entry.tool.output`, in UTF-8 bytes; the tail is kept.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024;

/// Ceiling on identifiers and names (`id`, `provider`, `tool.name`,
/// `tool.call_id`), in UTF-8 bytes.
pub const MAX_LABEL_BYTES: usize = 256;

/// Who an entry speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    /// The human's prompt.
    User,
    /// The agent's reply text.
    Assistant,
    /// The agent's visible reasoning.
    Thinking,
    /// One tool call; [`TranscriptEntry::tool`] carries it.
    Tool,
    /// A producer notice, such as a provider error.
    System,
}

/// Where one tool call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    /// Started and not yet finished.
    Running,
    /// Finished successfully.
    Ok,
    /// Finished with an error.
    Error,
}

/// The tool half of a `tool` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptTool {
    /// Tool name as the provider reports it.
    pub name: String,
    /// Provider call id; equal to the entry `id` for first-party producers.
    pub call_id: String,
    /// One-line summary of the arguments, at most [`MAX_TOOL_SUMMARY_CHARS`].
    pub summary: String,
    /// Where the call stands.
    pub status: ToolStatus,
    /// Tail of the result, at most [`MAX_TOOL_OUTPUT_BYTES`]; empty while running.
    pub output: String,
}

/// One conversation entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// Stable id; a later entry with the same id replaces this one.
    pub id: String,
    /// Who the entry speaks for.
    pub role: TranscriptRole,
    /// The entry text, cut on a character boundary to fit the record.
    pub text: String,
    /// Whether `text` was cut.
    pub truncated: bool,
    /// `false` for a streaming partial that a later entry replaces.
    #[serde(rename = "final")]
    pub is_final: bool,
    /// Present exactly when `role` is [`TranscriptRole::Tool`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<TranscriptTool>,
}

impl TranscriptEntry {
    /// Recognize a transcript entry in a `provider_raw` record's `data`.
    /// Anything without the [`TRANSCRIPT_SCHEMA`] word, or with an entry that
    /// does not decode, is `None`: other raw payloads pass through untouched.
    #[must_use]
    pub fn from_data(data: &Value) -> Option<Self> {
        if data.get("schema")?.as_str()? != TRANSCRIPT_SCHEMA {
            return None;
        }
        serde_json::from_value(data.get("entry")?.clone()).ok()
    }

    /// The `provider_raw` `data` object for this entry: control characters
    /// and terminal escape sequences stripped, every field within its
    /// ceiling, and the serialized object within
    /// [`MAX_TRANSCRIPT_DATA_BYTES`]. `text` is cut first (head kept,
    /// `truncated` set), then the tool output (tail kept).
    #[must_use]
    pub fn into_data(self, provider: &str) -> Value {
        let provider = label(provider);
        let mut entry = self.bounded();
        loop {
            let data = transcript_data(&provider, &entry);
            let over = serde_json::to_string(&data)
                .map_or(0, |encoded| encoded.len())
                .saturating_sub(MAX_TRANSCRIPT_DATA_BYTES);
            if over == 0 {
                return data;
            }
            if !entry.text.is_empty() {
                let keep = entry.text.len().saturating_sub(over);
                keep_head(&mut entry.text, keep);
                entry.truncated = true;
                continue;
            }
            match entry.tool.as_mut() {
                Some(tool) if !tool.output.is_empty() => {
                    let keep = tool.output.len().saturating_sub(over);
                    keep_tail(&mut tool.output, keep);
                }
                // Every remaining field is bounded far below the ceiling.
                _ => return data,
            }
        }
    }

    fn bounded(mut self) -> Self {
        self.id = label(&self.id);
        let cleaned = clean_text(&self.text);
        self.truncated |= cleaned.len() > MAX_TRANSCRIPT_DATA_BYTES;
        self.text = cleaned;
        keep_head(&mut self.text, MAX_TRANSCRIPT_DATA_BYTES);
        if self.role != TranscriptRole::Tool {
            self.tool = None;
        }
        if let Some(tool) = self.tool.as_mut() {
            tool.name = label(&tool.name);
            tool.call_id = label(&tool.call_id);
            tool.summary = one_line(&tool.summary, MAX_TOOL_SUMMARY_CHARS);
            tool.output = clean_text(&tool.output);
            keep_tail(&mut tool.output, MAX_TOOL_OUTPUT_BYTES);
        }
        self
    }
}

fn transcript_data(provider: &str, entry: &TranscriptEntry) -> Value {
    serde_json::json!({
        "provider": provider,
        "schema": TRANSCRIPT_SCHEMA,
        "entry": entry,
    })
}

/// Strip terminal escape sequences (CSI, OSC, and two-byte escapes) and every
/// control character except newline and tab. Carriage returns are dropped.
#[must_use]
pub fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
        } else if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters and intermediates, then one final byte.
        Some('[') => {
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        // OSC: up to BEL or ST (ESC \).
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\u{07}' {
                    break;
                }
                if c == '\u{1b}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        // Two-character escape, or a trailing ESC.
        _ => {}
    }
}

/// `text` cleaned, its whitespace runs collapsed to single spaces, and cut to
/// at most `max_chars` characters.
#[must_use]
pub fn one_line(text: &str, max_chars: usize) -> String {
    clean_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn label(text: &str) -> String {
    let mut text = one_line(text, MAX_LABEL_BYTES);
    keep_head(&mut text, MAX_LABEL_BYTES);
    text
}

/// Cut `text` to at most `max_bytes`, keeping the head, on a char boundary.
pub fn keep_head(text: &mut String, max_bytes: usize) {
    if text.len() > max_bytes {
        let cut = text.floor_char_boundary(max_bytes);
        text.truncate(cut);
    }
}

/// Cut `text` to at most `max_bytes`, keeping the tail, on a char boundary.
pub fn keep_tail(text: &mut String, max_bytes: usize) {
    if text.len() > max_bytes {
        let start = text.ceil_char_boundary(text.len() - max_bytes);
        text.drain(..start);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::session::agent_stream::parse_records;

    fn entry(role: TranscriptRole, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            id: "e-1".to_owned(),
            role,
            text: text.to_owned(),
            truncated: false,
            is_final: true,
            tool: None,
        }
    }

    fn tool_entry(output: &str) -> TranscriptEntry {
        TranscriptEntry {
            tool: Some(TranscriptTool {
                name: "bash".to_owned(),
                call_id: "call-1".to_owned(),
                summary: "ls -la\n  /tmp".to_owned(),
                status: ToolStatus::Ok,
                output: output.to_owned(),
            }),
            ..entry(TranscriptRole::Tool, "")
        }
    }

    /// The data object as a retained record line, the way the server would
    /// serialize it with the widest possible `seq` and `ts_ms`.
    fn retained_line(data: &Value) -> String {
        format!(
            "{{\"seq\":{},\"ts_ms\":{},\"type\":\"provider_raw\",\"data\":{data}}}\n",
            u64::MAX,
            u64::MAX
        )
    }

    #[test]
    fn data_has_the_documented_shape() {
        let data = entry(TranscriptRole::Assistant, "hello").into_data("pi");
        assert_eq!(
            data,
            serde_json::json!({
                "provider": "pi",
                "schema": "phux.transcript/v1",
                "entry": {
                    "id": "e-1",
                    "role": "assistant",
                    "text": "hello",
                    "truncated": false,
                    "final": true
                }
            })
        );
        let tool = tool_entry("done").into_data("claude");
        assert_eq!(
            tool["entry"]["tool"],
            serde_json::json!({
                "name": "bash",
                "call_id": "call-1",
                "summary": "ls -la /tmp",
                "status": "ok",
                "output": "done"
            })
        );
    }

    #[test]
    fn a_huge_text_is_cut_on_a_char_boundary_and_the_record_still_fits() {
        // Quotes double under JSON escaping; multi-byte chars test the boundary.
        let text = "\"é€😀".repeat(20_000);
        let data = entry(TranscriptRole::Assistant, &text).into_data("pi");
        let line = retained_line(&data);
        assert!(line.len() <= MAX_RECORD_BYTES, "{} bytes", line.len());
        let records = parse_records(line.as_bytes()).expect("a valid record");
        let decoded = records[0].transcript_entry().expect("a transcript entry");
        assert!(decoded.truncated);
        assert!(text.starts_with(&decoded.text), "the head is kept");
        assert!(decoded.text.len() > 8 * 1024, "the cut is not wasteful");
    }

    #[test]
    fn tool_output_keeps_its_tail_and_the_summary_its_cap() {
        let output = format!("{}TAIL", "x".repeat(10_000));
        let mut long = tool_entry(&output);
        if let Some(tool) = long.tool.as_mut() {
            tool.summary = "s".repeat(2_000);
        }
        let data = long.into_data("pi");
        let tool = &data["entry"]["tool"];
        let out = tool["output"].as_str().expect("output");
        assert_eq!(out.len(), MAX_TOOL_OUTPUT_BYTES);
        assert!(out.ends_with("TAIL"));
        let summary = tool["summary"].as_str().expect("summary");
        assert_eq!(summary.chars().count(), MAX_TOOL_SUMMARY_CHARS);
        assert_eq!(data["entry"]["truncated"], false, "text was not cut");
    }

    #[test]
    fn escape_sequences_and_controls_are_stripped() {
        let raw = "\u{1b}[1;31mred\u{1b}[0m \u{1b}]0;title\u{07}ok\r\n\ttab\u{0}\u{7f}\u{1b}]8;;x\u{1b}\\!";
        assert_eq!(clean_text(raw), "red ok\n\ttab!");
    }

    #[test]
    fn keep_head_and_tail_never_split_a_char() {
        let mut head = "a€b".to_owned();
        keep_head(&mut head, 2);
        assert_eq!(head, "a");
        let mut tail = "a€b".to_owned();
        keep_tail(&mut tail, 3);
        assert_eq!(tail, "b");
        let mut fits = "abc".to_owned();
        keep_tail(&mut fits, 3);
        assert_eq!(fits, "abc");
    }

    #[test]
    fn a_tool_field_on_a_non_tool_role_is_dropped() {
        let mut odd = tool_entry("x");
        odd.role = TranscriptRole::User;
        let data = odd.into_data("pi");
        assert!(data["entry"].get("tool").is_none());
    }

    #[test]
    fn recognition_requires_the_schema_word() {
        let data = entry(TranscriptRole::User, "hi").into_data("claude");
        assert_eq!(
            TranscriptEntry::from_data(&data).map(|e| e.text),
            Some("hi".to_owned())
        );
        let mut other = data.clone();
        other["schema"] = Value::from("phux.transcript/v2");
        assert_eq!(TranscriptEntry::from_data(&other), None);
        assert_eq!(
            TranscriptEntry::from_data(&serde_json::json!({"hook_event_name": "Stop"})),
            None
        );
        let mut bad_role = data;
        bad_role["entry"]["role"] = Value::from("narrator");
        assert_eq!(TranscriptEntry::from_data(&bad_role), None);
    }
}
