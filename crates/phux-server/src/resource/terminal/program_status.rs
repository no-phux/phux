//! Bounded, atomic OSC 7501 reports and per-terminal records.
//!
//! Records are stored oldest-to-newest, so recency and LRU eviction need no
//! clock or counter. Inherited apps are resolved only in consumer snapshots.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private terminal module shared by sibling runtime and state modules"
)]

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::Serialize;

const MAX_SEQUENCE_LEN: usize = 4096;
// ESC ] 7501 ; (seven bytes), plus the shorter terminator, BEL.
const MAX_BODY_LEN: usize = MAX_SEQUENCE_LEN - 8;
const MAX_KEY_LEN: usize = 16;
const MAX_MSG_ENCODED: usize = 2732;
const MAX_MSG_DECODED: usize = 2048;
const MAX_TITLE_ENCODED: usize = 256;
const MAX_TITLE_DECODED: usize = 192;
const MAX_APP_LEN: usize = 32;
const MAX_ID_LEN: usize = 128;
const MAX_SEGMENT_LEN: usize = 32;
const MAX_ID_DEPTH: usize = 8;
const MAX_RECORDS: usize = 256;
const STATES: [&str; 5] = ["blocked", "error", "done", "working", "idle"];
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Consumer-visible record. The root id is always the empty string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) msg: Option<String>,
}

/// Authoritative records for one terminal, independent of its active screen.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatus {
    records: Vec<Record>,
}

impl ProgramStatus {
    /// Apply a report body, atomically replacing the addressed record.
    ///
    /// False means ignored or no visible change. An accepted identical report
    /// still refreshes recency, which can reorder equal-priority records and
    /// always protects the record from least-recently-updated eviction.
    pub(crate) fn apply(&mut self, body: &str) -> bool {
        let Some(record) = parse_report(body) else {
            return false;
        };
        if record.state == "clear" {
            if record.id.is_empty() {
                return self.clear();
            }
            let before = self.records.len();
            self.records.retain(|stored| {
                stored.id != record.id
                    && !stored
                        .id
                        .strip_prefix(&record.id)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            });
            return self.records.len() != before;
        }

        let changed = if let Some(index) = self.records.iter().position(|r| r.id == record.id) {
            let changed = self.records[index] != record
                || self.records[index + 1..]
                    .iter()
                    .any(|later| later.state == record.state);
            self.records.remove(index);
            changed
        } else {
            if self.records.len() == MAX_RECORDS {
                self.records.remove(0);
            }
            true
        };
        self.records.push(record);
        changed
    }

    /// Prompt start or attached-process exit expires working and blocked only.
    pub(crate) fn expire_active(&mut self) -> bool {
        let before = self.records.len();
        self.records
            .retain(|record| !matches!(record.state, "working" | "blocked"));
        self.records.len() != before
    }

    /// Whether OSC 7501 currently owns any status on this terminal.
    pub(crate) const fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Full reset clears every record; alternate-screen and soft resets do not.
    pub(crate) fn clear(&mut self) -> bool {
        let changed = !self.records.is_empty();
        self.records.clear();
        changed
    }

    /// Priority descending, then most recently updated; apps resolve at read.
    pub(crate) fn snapshot(&self) -> Vec<Record> {
        let mut records = Vec::with_capacity(self.records.len());
        for state in STATES {
            for stored in self.records.iter().rev().filter(|r| r.state == state) {
                let mut record = stored.clone();
                if record.app.is_none() {
                    record.app = self.inherited_app(&record.id).map(str::to_owned);
                }
                records.push(record);
            }
        }
        records
    }

    /// Upgrade seed: reports oldest-to-newest, with stored (not inherited) app.
    ///
    /// Feed these bytes through the scanner and normal `apply` on a fresh
    /// record set. Both inheritance and the exact recency order are preserved.
    pub(crate) fn replay(&self) -> Vec<u8> {
        let mut bytes = String::new();
        for record in &self.records {
            bytes.push_str("\x1b]7501;state=");
            bytes.push_str(record.state);
            if !record.id.is_empty() {
                append_pair(&mut bytes, "id", &record.id);
            }
            if let Some(kind) = record.kind {
                append_pair(&mut bytes, "kind", kind);
            }
            if let Some(progress) = record.progress {
                // String's formatter is infallible.
                let _ = write!(bytes, ":progress={progress}");
            }
            if let Some(app) = &record.app {
                append_pair(&mut bytes, "app", app);
            }
            for (key, value) in [("title", &record.title), ("msg", &record.msg)] {
                if let Some(text) = value {
                    bytes.push(':');
                    bytes.push_str(key);
                    bytes.push('=');
                    BASE64.encode_string(text, &mut bytes);
                }
            }
            bytes.push_str("\x1b\\");
        }
        bytes.into_bytes()
    }

    fn inherited_app(&self, id: &str) -> Option<&str> {
        let mut ancestor = id;
        while !ancestor.is_empty() {
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
            if let Some(app) = self
                .records
                .iter()
                .find(|record| record.id == ancestor)
                .and_then(|record| record.app.as_deref())
            {
                return Some(app);
            }
        }
        None
    }
}

fn append_pair(output: &mut String, key: &str, value: &str) {
    output.push(':');
    output.push_str(key);
    output.push('=');
    output.push_str(value);
}

/// Validate every pair before allocating a record or touching stored state.
fn parse_report(body: &str) -> Option<Record> {
    if body.len() > MAX_BODY_LEN {
        return None;
    }
    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    // decode_slice requires room for its conservative decoded-length estimate,
    // including up to two bytes that padding removes from the actual output.
    let mut title_bytes = [0; MAX_TITLE_DECODED + 2];
    let mut msg_bytes = [0; MAX_MSG_DECODED + 2];
    let mut title_len = None;
    let mut msg_len = None;

    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.len() > MAX_KEY_LEN || exceeds_value_limit(key, value) {
            return None;
        }
        if key.is_empty()
            || !key.bytes().all(|byte| byte.is_ascii_lowercase())
            || !value.bytes().all(is_value_byte)
        {
            continue;
        }
        match key {
            "state" => state = Some(value),
            "id" => id = Some(value),
            "kind" => kind = Some(value),
            "progress" => progress = Some(value),
            "app" => app = Some(value),
            "title" => {
                title_len = Some(decode_text(value, &mut title_bytes, MAX_TITLE_DECODED)?);
            }
            "msg" => msg_len = Some(decode_text(value, &mut msg_bytes, MAX_MSG_DECODED)?),
            _ => {}
        }
    }

    let word = state?;
    let state = STATES
        .iter()
        .copied()
        .find(|known| *known == word)
        .or_else(|| (word == "clear").then_some("clear"))?;
    if id.is_some_and(|id| !valid_id(id)) {
        return None;
    }
    let active = matches!(state, "working" | "blocked");
    Some(Record {
        id: id.unwrap_or_default().to_owned(),
        state,
        kind: match (state, kind) {
            ("blocked", Some("permission")) => Some("permission"),
            ("blocked", Some("question")) => Some("question"),
            ("blocked", Some("auth")) => Some("auth"),
            _ => None,
        },
        progress: progress
            .filter(|value| {
                active && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
            })
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|value| *value <= 100),
        app: app.filter(|app| valid_segment(app)).map(str::to_owned),
        title: title_len
            .map(|len| std::str::from_utf8(&title_bytes[..len]).map(str::to_owned))
            .transpose()
            .ok()?,
        msg: msg_len
            .map(|len| std::str::from_utf8(&msg_bytes[..len]).map(str::to_owned))
            .transpose()
            .ok()?,
    })
}

fn exceeds_value_limit(key: &str, value: &str) -> bool {
    match key {
        "title" => value.len() > MAX_TITLE_ENCODED,
        "msg" => value.len() > MAX_MSG_ENCODED,
        "app" => value.len() > MAX_APP_LEN,
        "id" => {
            value.len() > MAX_ID_LEN
                || value.split('/').count() > MAX_ID_DEPTH
                || value.split('/').any(|part| part.len() > MAX_SEGMENT_LEN)
        }
        _ => false,
    }
}

fn decode_text(value: &str, buffer: &mut [u8], limit: usize) -> Option<usize> {
    // Optional padding means either canonical padding or none, not a partial
    // suffix. The engine's Indifferent mode alone also accepts partial padding.
    if let Some(first) = value.find('=') {
        let padding = &value[first..];
        if !value.len().is_multiple_of(4)
            || !(1..=2).contains(&padding.len())
            || !padding.bytes().all(|byte| byte == b'=')
        {
            return None;
        }
    }
    let len = BASE64.decode_slice(value, buffer).ok()?;
    if len > limit {
        return None;
    }
    let text = std::str::from_utf8(&buffer[..len]).ok()?;
    if text
        .chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
    {
        return None;
    }
    Some(len)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.split('/').all(valid_segment)
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty() && segment.len() <= MAX_SEGMENT_LEN && segment.bytes().all(is_segment_byte)
}

const fn is_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-')
}

const fn is_value_byte(byte: u8) -> bool {
    is_segment_byte(byte) || matches!(byte, b',' | b'/' | b'=')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::terminal::osc133::{Osc133Scanner, OscMark};

    fn record<'a>(records: &'a [Record], id: &str) -> &'a Record {
        records.iter().find(|r| r.id == id).expect("record exists")
    }

    fn assert_ignored(status: &mut ProgramStatus, body: &str) {
        let before = status.snapshot();
        let replay = status.replay();
        assert!(!status.apply(body), "unexpectedly accepted {body:?}");
        assert_eq!(status.snapshot(), before, "report must be atomic");
        assert_eq!(
            status.replay(),
            replay,
            "ignored reports must not refresh recency"
        );
    }

    #[test]
    fn replacement_and_json_preserve_only_the_last_report() {
        let mut status = ProgramStatus::default();
        assert!(status.apply(
            "state=blocked:kind=permission:progress=42:app=cargo:title=QnVpbGQ=:msg=QXBwcm92ZT8="
        ));
        let root = status.snapshot().remove(0);
        assert_eq!(root.id, "");
        assert_eq!(root.kind, Some("permission"));
        assert_eq!(root.progress, Some(42));
        assert_eq!(root.title.as_deref(), Some("Build"));
        assert_eq!(root.msg.as_deref(), Some("Approve?"));
        assert!(status.apply("state=done"));
        assert_eq!(
            serde_json::to_value(status.snapshot().remove(0)).unwrap(),
            serde_json::json!({"id": "", "state": "done"})
        );
        assert!(!status.apply("state=done"));
    }

    #[test]
    fn pairs_trim_skip_malformed_ignore_unknown_and_last_valid_pair_wins() {
        let mut status = ProgramStatus::default();
        assert!(status.apply(
            " state = working :broken:=bad:State=error:msg=bad value:future=x:state=blocked:state=done:msg=Zmlyc3Q=:msg=bGFzdA"
        ));
        let root = status.snapshot().remove(0);
        assert_eq!(root.state, "done");
        assert_eq!(root.msg.as_deref(), Some("last"));
        assert_ignored(&mut status, "id=x:msg=b2s=");
        assert_ignored(&mut status, "state=done:state=future");
        assert_ignored(&mut status, "?");
        assert!(status.apply("state=future:state=idle"));
        assert!(status.apply("state=done:id=bad!:id=child"));
        assert_eq!(status.snapshot()[0].id, "child");
        assert!(
            status.apply("state=error:id=bad!"),
            "malformed value pair is skipped"
        );
        assert_eq!(record(&status.snapshot(), "").state, "error");
        assert!(
            status.apply("state=working:id=a//b:id=child"),
            "last id wins"
        );
        assert!(
            status.apply("state=idle:malformed_pair_without_an_equals_sign"),
            "a pair without '=' has no key whose length can exceed the key cap"
        );
        assert_eq!(record(&status.snapshot(), "").state, "idle");
    }

    #[test]
    fn optional_values_are_absent_unless_recognized_and_applicable() {
        let mut status = ProgramStatus::default();
        for state in STATES {
            assert!(status.apply(&format!(
                "state={state}:kind=auth:progress=100:app=tool-1.2+_"
            )));
            let root = status.snapshot().remove(0);
            assert_eq!(root.kind, (state == "blocked").then_some("auth"));
            assert_eq!(
                root.progress,
                matches!(state, "working" | "blocked").then_some(100)
            );
            assert_eq!(root.app.as_deref(), Some("tool-1.2+_"));
        }
        for value in [
            "",
            "101",
            "-1",
            "+1",
            "1.0",
            "1,0",
            "unknown",
            "99999999999999999",
        ] {
            status.apply(&format!(
                "state=working:kind=future:progress={value}:app=a/b"
            ));
            let root = status.snapshot().remove(0);
            assert_eq!(root.kind, None);
            assert_eq!(root.progress, None, "{value}");
            assert_eq!(root.app, None);
        }
        for (value, expected) in [("0", 0), ("001", 1), ("99", 99), ("100", 100)] {
            status.apply(&format!("state=blocked:progress={value}"));
            assert_eq!(status.snapshot()[0].progress, Some(expected));
        }
    }

    #[test]
    fn invalid_ids_do_not_overwrite_root_and_hard_limits_check_losing_pairs() {
        let mut status = ProgramStatus::default();
        status.apply("state=done:app=root");
        for id in ["", "/a", "a/", "a//b", "a,b", "a=b"] {
            assert_ignored(&mut status, &format!("state=error:id={id}"));
        }
        for body in [
            format!("state=idle:{}=x", "k".repeat(MAX_KEY_LEN + 1)),
            format!("state=idle:app={}:app=ok", "a".repeat(MAX_APP_LEN + 1)),
            format!("state=idle:id={}:id=ok", "a".repeat(MAX_SEGMENT_LEN + 1)),
            format!("state=idle:id={}:id=ok", ["a"; MAX_ID_DEPTH + 1].join("/")),
            format!(
                "state=idle:id={}:id=ok",
                ["a".repeat(25).as_str(); 5].join("/")
            ),
            format!(
                "state=idle:title={}:title=b2s=",
                "A".repeat(MAX_TITLE_ENCODED + 1)
            ),
            format!(
                "state=idle:msg={}:msg=b2s=",
                "A".repeat(MAX_MSG_ENCODED + 1)
            ),
        ] {
            assert_ignored(&mut status, &body);
        }
        assert!(status.apply(&format!("state=idle:{}=ignored", "k".repeat(MAX_KEY_LEN))));
        assert!(status.apply(&format!("state=working:app={}", "a".repeat(MAX_APP_LEN))));
        assert!(status.apply(&format!("state=working:id={}", "x".repeat(MAX_SEGMENT_LEN))));
        assert!(status.apply(&format!(
            "state=working:id={}",
            ["a"; MAX_ID_DEPTH].join("/")
        )));
        let max_id = format!(
            "{}/{}",
            "x".repeat(32),
            ["y".repeat(31).as_str(); 3].join("/")
        );
        assert_eq!(max_id.len(), MAX_ID_LEN);
        assert!(status.apply(&format!("state=working:id={max_id}")));
    }

    #[test]
    fn standard_base64_is_strict_padded_or_unpadded_and_utf8_control_free() {
        let mut status = ProgramStatus::default();
        for text in ["", "a", "ab", "abc", "café 🦀", "one line / + ="] {
            let encoded = BASE64.encode(text);
            for encoded in [encoded.as_str(), encoded.trim_end_matches('=')] {
                status.apply(&format!("state=done:msg={encoded}:title={encoded}"));
                let root = status.snapshot().remove(0);
                assert_eq!(root.msg.as_deref(), Some(text));
                assert_eq!(root.title.as_deref(), Some(text));
            }
        }
        for invalid in [
            "A", "A===", "YQ=", "YQ===", "YR==", "YR", "YWJ=", "YWJ", "AA=A", "_w==", "-w==",
            "/w==",
        ] {
            assert_ignored(&mut status, &format!("state=error:msg={invalid}:msg=b2s="));
            assert_ignored(
                &mut status,
                &format!("state=clear:title={invalid}:title=b2s="),
            );
        }
        for code in (0..=0x1f).chain(0x7f..=0x9f) {
            let control = char::from_u32(code).unwrap();
            let encoded = BASE64.encode(format!("safe{control}text"));
            assert_ignored(&mut status, &format!("state=error:msg={encoded}:msg=b2s="));
            assert_ignored(
                &mut status,
                &format!("state=clear:title={encoded}:title=b2s="),
            );
        }
    }

    #[test]
    fn encoded_decoded_and_whole_body_boundaries_are_atomic() {
        let mut status = ProgramStatus::default();
        let msg = "m".repeat(MAX_MSG_DECODED);
        let title = "t".repeat(MAX_TITLE_DECODED);
        assert_eq!(BASE64.encode(&msg).len(), MAX_MSG_ENCODED);
        assert_eq!(BASE64.encode(&title).len(), MAX_TITLE_ENCODED);
        assert!(status.apply(&format!(
            "state=done:msg={}:title={}",
            BASE64.encode(&msg),
            BASE64.encode(&title)
        )));
        assert_eq!(status.snapshot()[0].msg.as_deref(), Some(msg.as_str()));
        assert_eq!(status.snapshot()[0].title.as_deref(), Some(title.as_str()));
        // 2049 decoded bytes still fit the encoded cap, so decoded size matters.
        let too_big = BASE64.encode("m".repeat(MAX_MSG_DECODED + 1));
        assert_eq!(too_big.len(), MAX_MSG_ENCODED);
        assert_ignored(&mut status, &format!("state=error:msg={too_big}:msg=b2s="));
        let prefix = "state=idle:x=";
        let max_body = format!("{prefix}{}", "a".repeat(MAX_BODY_LEN - prefix.len()));
        assert!(status.apply(&max_body));
        assert_ignored(&mut status, &(max_body + "a"));
    }

    #[test]
    fn nearest_ancestor_app_is_live_in_snapshots_and_never_written_to_children() {
        let mut status = ProgramStatus::default();
        status.apply("state=idle:app=root");
        status.apply("state=working:id=build:app=cargo");
        status.apply("state=blocked:id=build/test/unit");
        status.apply("state=done:id=other/leaf");
        status.apply("state=done:id=isolated:app=own");
        let records = status.snapshot();
        assert_eq!(
            record(&records, "build/test/unit").app.as_deref(),
            Some("cargo")
        );
        assert_eq!(record(&records, "other/leaf").app.as_deref(), Some("root"));
        assert_eq!(record(&records, "isolated").app.as_deref(), Some("own"));
        assert_eq!(record(&status.records, "build/test/unit").app, None);
        status.apply("state=working:id=build");
        assert_eq!(
            record(&status.snapshot(), "build/test/unit").app.as_deref(),
            Some("root")
        );
        status.apply("state=idle:app=new-root");
        assert_eq!(
            record(&status.snapshot(), "build/test/unit").app.as_deref(),
            Some("new-root")
        );
        status.apply("state=idle");
        assert_eq!(record(&status.snapshot(), "build/test/unit").app, None);
    }

    #[test]
    fn subtree_clear_obeys_path_boundaries_and_root_clear_removes_everything() {
        let mut status = ProgramStatus::default();
        for id in ["a", "a/b", "a/b/c", "a/bc", "ab", "x/a/b"] {
            status.apply(&format!("state=done:id={id}"));
        }
        assert!(status.apply("state=clear:id=a/b"));
        let ids: Vec<_> = status.snapshot().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["x/a/b", "ab", "a/bc", "a"]);
        assert!(!status.apply("state=clear:id=a/b"));
        assert!(status.apply("state=done:id=missing/child"));
        assert!(
            status.apply("state=clear:id=missing"),
            "parent need not exist"
        );
        assert!(status.apply("state=clear"));
        assert!(status.snapshot().is_empty());
        assert!(!status.clear());
    }

    #[test]
    fn expiry_keeps_idle_done_error_and_can_change_inherited_apps() {
        let mut status = ProgramStatus::default();
        status.apply("state=working:app=parent");
        for state in STATES {
            status.apply(&format!("state={state}:id={state}"));
        }
        assert!(status.expire_active());
        let records = status.snapshot();
        assert_eq!(
            records.iter().map(|r| r.state).collect::<Vec<_>>(),
            ["error", "done", "idle"]
        );
        assert!(records.iter().all(|r| r.app.is_none()));
        assert!(!status.expire_active());
        assert!(status.clear());
    }

    #[test]
    fn priority_and_identical_refresh_are_visible_only_when_order_changes() {
        let mut status = ProgramStatus::default();
        for state in STATES {
            status.apply(&format!("state={state}:id={state}"));
        }
        assert_eq!(
            status
                .snapshot()
                .iter()
                .map(|r| r.state)
                .collect::<Vec<_>>(),
            STATES
        );
        assert!(
            !status.apply("state=blocked:id=blocked"),
            "only later lower-priority records"
        );
        assert!(status.apply("state=blocked:id=new"));
        assert!(status.apply("state=blocked:id=blocked"));
        assert_eq!(status.snapshot()[0].id, "blocked");
        assert_eq!(status.snapshot()[1].id, "new");
        assert!(!status.apply("state=blocked:id=blocked"));
    }

    #[test]
    fn lru_eviction_is_by_update_not_priority_and_identical_reports_refresh_it() {
        let mut status = ProgramStatus::default();
        for index in 0..MAX_RECORDS {
            status.apply(&format!("state=blocked:id=r{index}"));
        }
        assert_eq!(status.records.len(), MAX_RECORDS);
        assert!(status.apply("state=blocked:id=r0"));
        assert!(status.apply("state=idle:id=new"));
        assert_eq!(status.records.len(), MAX_RECORDS);
        assert!(status.records.iter().any(|r| r.id == "r0"));
        assert!(!status.records.iter().any(|r| r.id == "r1"));
        // An invalid report cannot refresh the next eviction candidate.
        assert_ignored(&mut status, "state=blocked:id=r2:msg=YR");
        assert!(status.apply("state=done:id=newer"));
        assert!(!status.records.iter().any(|r| r.id == "r2"));
    }

    #[test]
    fn replay_restores_own_apps_unicode_empty_fields_and_recency() {
        let mut status = ProgramStatus::default();
        status.apply("state=idle:app=root");
        status.apply("state=working:id=parent:app=near:progress=100");
        assert!(status.apply(&format!(
            "state=blocked:id=parent/child:kind=question:msg={}",
            BASE64.encode("café 🦀")
        )));
        status.apply("state=done:id=finished:title=:msg=");
        status.apply("state=working:id=parent:app=near:progress=100");
        let replay = status.replay();
        let mut scanner = Osc133Scanner::new();
        let mut restored = ProgramStatus::default();
        for byte in replay {
            for mark in scanner.feed(&[byte]) {
                let OscMark::ProgramStatus(body) = mark else {
                    panic!("replay contains reports only");
                };
                restored.apply(&body);
            }
        }
        assert_eq!(restored.records, status.records);
        assert_eq!(restored.snapshot(), status.snapshot());
        restored.apply("state=working:id=parent");
        assert_eq!(
            record(&restored.snapshot(), "parent/child").app.as_deref(),
            Some("root")
        );
    }

    #[test]
    fn malformed_utf8_pairs_skip_without_losing_other_valid_pairs() {
        let mut scanner = Osc133Scanner::new();
        let mut status = ProgramStatus::default();
        assert!(status.is_empty());
        status.apply("state=working:app=root");
        assert!(!status.is_empty());
        for mark in scanner.feed(b"\x1b]7501;state=done:future=\xff\x07") {
            let OscMark::ProgramStatus(body) = mark else {
                panic!("expected report");
            };
            assert!(status.apply(&body));
        }
        assert_eq!(status.snapshot()[0].state, "done");
        status.clear();
        assert!(status.is_empty());
    }
}
