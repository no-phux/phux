//! A follower across a replacement generation: overlap is deduplicated and
//! an evicted window ends the follow with the gap.

use super::*;

fn record_line(seq: u64) -> String {
    format!("{{\"seq\":{seq},\"ts_ms\":{seq},\"type\":\"stop\",\"data\":{{}}}}\n")
}

/// The frames a server sends when a lagged follower's generation is retired:
/// a gap tombstone, then a full retained replacement under a new id.
fn replacement(
    spec: ScriptSpec,
    last_valid_seq: u64,
    retained: std::ops::RangeInclusive<u64>,
) -> ScriptSpec {
    let stream_id = phux_protocol::ids::StreamId::new(1).unwrap();
    let old = phux_protocol::ids::BootstrapId::new(1).unwrap();
    let new = phux_protocol::ids::BootstrapId::new(2).unwrap();
    let base_seq = *retained.end();
    let payload: String = retained.map(record_line).collect();
    spec.push(FrameKind::BootstrapTombstone {
        terminal_id: session(),
        stream_id,
        bootstrap_id: old,
        reason: phux_protocol::wire::frame::TombstoneReason::OutboundGap,
        last_valid_seq,
    })
    .push(FrameKind::BootstrapBegin {
        terminal_id: session(),
        stream_id,
        bootstrap_id: new,
        profile: phux_protocol::caps::BootstrapStreamProfile::AgentEventsJsonlV1,
        cols: 0,
        rows: 0,
        base_seq,
    })
    .push(FrameKind::BootstrapChunk {
        terminal_id: session(),
        stream_id,
        bootstrap_id: new,
        chunk_seq: 0,
        payload: bytes::Bytes::from(payload),
    })
    .push(FrameKind::BootstrapReady {
        terminal_id: session(),
        stream_id,
        bootstrap_id: new,
        history_cursor: None,
    })
}

async fn follow_seqs(spec: ScriptSpec) -> (Vec<u64>, LogEnd) {
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut seqs = Vec::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        log(
            &mut conn,
            &session(),
            LogOptions {
                follow: true,
                tail: None,
            },
            |record| {
                seqs.push(record.seq);
                true
            },
        ),
    )
    .await
    .expect("the follow ends")
    .unwrap();
    drop(conn);
    server.await.unwrap();
    (seqs, outcome.end)
}

#[tokio::test]
async fn a_replacement_generation_overlapping_the_follow_delivers_no_duplicate() {
    let first = record_line(1);
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap(&session(), &[first.trim_end()])
        .push(live_frame(record_line(2).as_bytes()));
    let spec = replacement(spec, 2, 2..=4).push(closed_frame());
    let (seqs, end) = follow_seqs(spec).await;
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_eq!(end, LogEnd::SessionClosed);
}

#[tokio::test]
async fn a_replacement_after_evicted_records_ends_the_follow_with_the_gap() {
    let first = record_line(1);
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap(&session(), &[first.trim_end()])
        .push(live_frame(record_line(2).as_bytes()));
    let spec = replacement(spec, 2, 6..=7).push(closed_frame());
    let (seqs, end) = follow_seqs(spec).await;
    assert_eq!(seqs, [1, 2], "nothing past the gap is delivered");
    assert_eq!(
        end,
        LogEnd::RecordsLost {
            after_seq: 2,
            next_seq: 6
        }
    );
}
