//! `AgentSession` records (ADR-0103) as the codec's own JSON lines.
//!
//! The kernel validated and decoded every record; both encoders hand them
//! on as `AgentEventsJsonlV1` lines (`{"seq","ts_ms","type","data"}`, one
//! object per line, each ending in `\n`) so a host parses one documented
//! shape, including the `phux.transcript/v1` entries `provider_raw` records
//! carry (ADR-0156).

use phux_client_core::session::agent_stream::AgentEventRecord;

/// `records` as JSON lines; empty for no records.
#[must_use]
pub fn jsonl(records: &[AgentEventRecord]) -> String {
    let mut lines = String::new();
    for record in records {
        let line = serde_json::json!({
            "seq": record.seq,
            "ts_ms": record.ts_ms,
            "type": record.kind.as_str(),
            "data": record.data,
        });
        lines.push_str(&line.to_string());
        lines.push('\n');
    }
    lines
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
