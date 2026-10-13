//! `AgentSession` records (ADR-0103) as the codec's own JSON lines.
//!
//! The kernel validated and decoded every record; both encoders hand them
//! on as `AgentEventsJsonlV1` lines (`{"seq","ts_ms","type","data"}`, one
//! object per line, each ending in `\n`) so a host parses one documented
//! shape, including the `phux.transcript/v1` entries `provider_raw` records
//! carry (ADR-0156).

use phux_client_core::session::AgentStreamInfo;
use phux_client_core::session::agent_stream::{
    AgentEventRecord, AgentPublication, AgentRecordMode,
};

/// One representation-neutral foreign record batch.
pub(crate) struct RecordBatch {
    pub mode: AgentRecordMode,
    pub stream: AgentStreamInfo,
    pub jsonl: String,
    pub seq: u64,
}

/// Empty appends advance identity only; empty replacements clear evidence.
pub(crate) fn project(publication: &AgentPublication) -> Option<RecordBatch> {
    if publication.mode == AgentRecordMode::Append && publication.records.is_empty() {
        return None;
    }
    Some(RecordBatch {
        mode: publication.mode,
        stream: publication.stream,
        seq: publication.records.last().map_or(0, |record| record.seq),
        jsonl: jsonl(&publication.records),
    })
}

/// `records` as JSON lines; empty for no records.
///
/// # Panics
///
/// Panics if the JSON encoder violates its in-memory writer or UTF-8 invariants.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "borrowed JSON values serialize infallibly into a Vec and the serializer emits UTF-8"
)]
pub fn jsonl(records: &[AgentEventRecord]) -> String {
    let mut lines = Vec::new();
    for record in records {
        lines.extend_from_slice(b"{\"data\":");
        serde_json::to_writer(&mut lines, &record.data)
            .expect("JSON data serializes into a byte buffer");
        lines.extend_from_slice(b",\"seq\":");
        serde_json::to_writer(&mut lines, &record.seq)
            .expect("record sequence serializes into a byte buffer");
        lines.extend_from_slice(b",\"ts_ms\":");
        serde_json::to_writer(&mut lines, &record.ts_ms)
            .expect("record timestamp serializes into a byte buffer");
        lines.extend_from_slice(b",\"type\":");
        serde_json::to_writer(&mut lines, record.kind.as_str())
            .expect("record kind serializes into a byte buffer");
        lines.extend_from_slice(b"}\n");
    }
    String::from_utf8(lines).expect("JSON serialization produces UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_is_one_codec_line() {
        assert_eq!(jsonl(&[]), "");
        let records = phux_client_core::session::agent_stream::parse_records(
            br#"{"seq":1,"ts_ms":10,"type":"prompt","data":{"length":3}}
{"seq":2,"ts_ms":20,"type":"stop","data":{}}
"#,
        )
        .expect("records parse");
        let encoded = jsonl(&records);
        assert!(encoded.ends_with('\n'));
        let lines: Vec<serde_json::Value> = encoded
            .lines()
            .map(|line| serde_json::from_str(line).expect("line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["seq"], 1);
        assert_eq!(lines[0]["ts_ms"], 10);
        assert_eq!(lines[0]["type"], "prompt");
        assert_eq!(lines[0]["data"]["length"], 3);
        assert_eq!(lines[1]["type"], "stop");
    }
}
