use super::*;

fn one(line: &str) -> Result<ValidRecord, RecordError> {
    validate(line.as_bytes(), false).map(|mut r| r.remove(0))
}

#[test]
fn a_batch_splits_on_newlines_with_an_optional_trailing_one() {
    let payload = "{\"type\":\"prompt\"}\n{\"type\":\"stop\",\"data\":{}}";
    let records = validate(payload.as_bytes(), false).expect("two records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].record_type, "stop");
}

#[test]
fn blank_segments_are_skipped_and_an_empty_append_is_refused() {
    assert_eq!(
        validate(b"\n\n  \n", false),
        Err(RecordError::Empty),
        "whitespace alone carries no record"
    );
    assert_eq!(validate(b"", false), Err(RecordError::Empty));
    let records = validate(b"\n{\"type\":\"stop\"}\n\n", false).expect("one record");
    assert_eq!(records.len(), 1);
}

#[test]
fn the_closed_type_set_is_enforced() {
    assert_eq!(
        one("{\"type\":\"tool_middle\"}"),
        Err(RecordError::UnknownType {
            found: "tool_middle".to_owned()
        })
    );
    for record_type in RECORD_TYPES {
        one(&format!("{{\"type\":\"{record_type}\"}}"))
            .unwrap_or_else(|e| panic!("{record_type} is v1: {e}"));
    }
}

#[test]
fn a_record_needs_a_string_type_an_object_body_and_an_object_data() {
    assert_eq!(one("[1,2]"), Err(RecordError::NotAnObject));
    assert_eq!(one("not json"), Err(RecordError::NotAnObject));
    assert_eq!(one("{\"kind\":\"prompt\"}"), Err(RecordError::MissingType));
    assert_eq!(one("{\"type\":7}"), Err(RecordError::MissingType));
    assert_eq!(
        one("{\"type\":\"prompt\",\"data\":[]}"),
        Err(RecordError::DataNotObject)
    );
    assert_eq!(
        one("{\"type\":\"prompt\",\"data\":null}")
            .expect("null data normalizes")
            .data
            .len(),
        0
    );
}

#[test]
fn oversize_and_post_session_end_records_are_refused() {
    let big = format!(
        "{{\"type\":\"provider_raw\",\"data\":{{\"b\":\"{}\"}}}}",
        "x".repeat(MAX_RECORD_BYTES)
    );
    assert!(matches!(
        validate(big.as_bytes(), false),
        Err(RecordError::Oversized { .. })
    ));
    assert_eq!(
        validate(b"{\"type\":\"prompt\"}", true),
        Err(RecordError::AfterSessionEnd)
    );
    assert_eq!(
        validate(b"{\"type\":\"session_end\"}\n{\"type\":\"prompt\"}", false),
        Err(RecordError::AfterSessionEnd),
        "a batch is checked against its own session_end"
    );
}

#[test]
fn state_derivation_follows_adr_0103() {
    let cases = [
        ("{\"type\":\"prompt\"}", Some(StreamEvidence::Working)),
        ("{\"type\":\"tool_start\"}", Some(StreamEvidence::Working)),
        ("{\"type\":\"tool_end\"}", None),
        ("{\"type\":\"ask\"}", Some(StreamEvidence::Blocked)),
        (
            "{\"type\":\"notification\",\"data\":{\"kind\":\"permission\"}}",
            Some(StreamEvidence::Blocked),
        ),
        (
            "{\"type\":\"notification\",\"data\":{\"kind\":\"elicitation\"}}",
            Some(StreamEvidence::Blocked),
        ),
        (
            "{\"type\":\"notification\",\"data\":{\"kind\":\"progress\"}}",
            None,
        ),
        ("{\"type\":\"stop\"}", Some(StreamEvidence::Done)),
        ("{\"type\":\"session_end\"}", Some(StreamEvidence::Retract)),
        ("{\"type\":\"session_start\"}", None),
        (
            "{\"type\":\"state\",\"data\":{\"state\":\"blocked\"}}",
            Some(StreamEvidence::Blocked),
        ),
        ("{\"type\":\"state\",\"data\":{\"state\":\"idle\"}}", None),
        ("{\"type\":\"provider_raw\"}", None),
    ];
    for (line, expected) in cases {
        assert_eq!(one(line).expect("valid").evidence(), expected, "{line}");
    }
}

/// Only a `stop` that names a non-blank reply asks for a reply push.
#[test]
fn a_reply_is_an_opt_in_on_stop_only() {
    let reply = |line: &str| one(line).expect("valid").reply();
    assert_eq!(
        reply(
            r#"{"type":"stop","data":{"reply":"done: 3 PRs","turn_id":"t1","reply_title":"Chief"}}"#
        ),
        Some(StreamReply {
            turn_id: "t1".to_owned(),
            text: "done: 3 PRs".to_owned(),
            title: Some("Chief".to_owned()),
        })
    );
    assert_eq!(
        reply(r#"{"type":"stop","data":{"reply":"ok","reply_title":" "}}"#),
        Some(StreamReply {
            turn_id: String::new(),
            text: "ok".to_owned(),
            title: None,
        })
    );
    assert_eq!(reply(r#"{"type":"stop","data":{}}"#), None);
    assert_eq!(reply(r#"{"type":"stop","data":{"reply":"  "}}"#), None);
    assert_eq!(reply(r#"{"type":"stop","data":{"reply":7}}"#), None);
    assert_eq!(
        reply(r#"{"type":"provider_raw","data":{"reply":"not a stop"}}"#),
        None
    );
}

#[test]
fn stamping_is_canonical_and_ignores_producer_seq_and_time() {
    let record =
        one("{\"seq\":99,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{\"len\":4}}").expect("valid");
    assert_eq!(
        record.stamp(7, 1_700_000_000_000),
        bytes::Bytes::from_static(
            b"{\"seq\":7,\"ts_ms\":1700000000000,\"type\":\"prompt\",\"data\":{\"len\":4}}\n"
        )
    );
}

/// The direct writer must stay byte-identical to `Value`'s serializer,
/// including escape sequences and object key order.
#[test]
fn stamping_matches_the_value_serializer_including_escapes() {
    let record = one(
        r#"{"type":"provider_raw","data":{"text":"say \"hi\"\n","n":1,"flag":true,"empty":{}}}"#,
    )
    .expect("valid");
    let seq = 3_u64;
    let ts_ms = 9_u64;
    let mut legacy = String::new();
    legacy.push_str("{\"seq\":");
    legacy.push_str(&seq.to_string());
    legacy.push_str(",\"ts_ms\":");
    legacy.push_str(&ts_ms.to_string());
    legacy.push_str(",\"type\":");
    legacy.push_str(&Value::String(record.record_type.clone()).to_string());
    legacy.push_str(",\"data\":");
    legacy.push_str(&Value::Object(record.data.clone()).to_string());
    legacy.push_str("}\n");
    assert_eq!(record.stamp(seq, ts_ms).as_ref(), legacy.as_bytes());
}

#[test]
fn canonical_record_limit_includes_newline_and_server_fields() {
    let mut record = one("{\"type\":\"provider_raw\",\"data\":{\"text\":\"\"}}").expect("valid");
    let overhead = record.stamp(u64::MAX, u64::MAX).len();
    record.data.insert(
        "text".to_owned(),
        Value::String("x".repeat(MAX_RECORD_BYTES - overhead)),
    );
    let line = record
        .stamp_bounded(u64::MAX, u64::MAX)
        .expect("exact bound");
    assert_eq!(line.len(), MAX_RECORD_BYTES);
    record.data["text"] = Value::String("x".repeat(MAX_RECORD_BYTES - overhead + 1));
    assert_eq!(
        record.stamp_bounded(u64::MAX, u64::MAX),
        Err(RecordError::Oversized {
            len: MAX_RECORD_BYTES + 1
        })
    );
}

#[test]
fn producer_record_limit_counts_lf_and_untrimmed_whitespace() {
    let mut line = "{\"type\":\"provider_raw\"}".to_owned();
    line.push_str(&" ".repeat(MAX_RECORD_BYTES - 1 - line.len()));
    for suffix in ["", "\n"] {
        assert!(validate(format!("{line}{suffix}").as_bytes(), false).is_ok());
        assert_eq!(
            validate(format!("{line} {suffix}").as_bytes(), false),
            Err(RecordError::Oversized {
                len: MAX_RECORD_BYTES + 1
            })
        );
    }
}
