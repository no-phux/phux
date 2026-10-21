use super::*;

/// Build a detached engine and drive it by hand, without the runtime.
fn engine() -> (AgentSessionActor, AgentSessionHandle, ResourceHandle) {
    let bundle = AgentSessionActor::build(
        ResourceId::default(),
        "claude",
        Some("abc"),
        CancellationToken::new(),
        4096,
    );
    let facet = match &bundle.handle.facet {
        ResourceFacetHandle::AgentSession(facet) => facet.clone(),
        ResourceFacetHandle::Terminal(_) => panic!("built the wrong facet"),
    };
    (bundle.actor, facet, bundle.handle)
}

#[test]
fn one_batch_broadcast_is_the_concatenation_of_its_lines() {
    let (mut actor, _facet, handle) = engine();
    let mut subscriber = handle.output.subscribe();
    append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
    let super::PaneOutput::Live { bytes, .. } = subscriber.try_recv().expect("broadcast") else {
        panic!("append publishes a live batch");
    };
    let replay = actor.bootstrap(None);
    let mut joined = Vec::new();
    for record in &replay.records {
        joined.extend_from_slice(record);
    }
    assert_eq!(bytes.as_ref(), joined.as_slice());
    assert_eq!(replay.records.len(), 2);
}

#[test]
fn icount_helper_appends_and_replays_the_ring() {
    let (mut actor, _facet, _handle) = engine();
    let replayed = icount_append_and_replay(
        &mut actor,
        Bytes::from_static(b"{\"type\":\"prompt\"}\n{\"type\":\"stop\"}\n"),
    )
    .expect("accepted");
    assert!(replayed > 0, "replay copies the stamped records");
    assert_eq!(actor.ring.len(), 2);
}

fn append(actor: &mut AgentSessionActor, payload: &str) -> Result<AppendAccepted, AppendRejection> {
    let (reply, rx) = oneshot::channel();
    actor.handle_append(AppendRequest {
        bytes: Bytes::copy_from_slice(payload.as_bytes()),
        reply,
        publication: None,
    });
    rx.blocking_recv().expect("the engine always replies")
}

fn cut(
    actor: &mut AgentSessionActor,
    after_seq: Option<u64>,
    seal: Option<bool>,
) -> AgentSessionBootstrap {
    let (reply, mut rx) = oneshot::channel();
    actor.handle_bootstrap(BootstrapRequest {
        reply,
        after_seq,
        seal,
    });
    rx.try_recv().expect("the actor replies synchronously")
}

#[test]
fn resume_selects_the_first_boundary_a_batch_suffix_and_an_empty_cut() {
    let (mut actor, _facet, _handle) = engine();
    append(
        &mut actor,
        "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
    )
    .expect("accepted batch");
    let full = cut(&mut actor, None, None);
    let boundary = cut(&mut actor, Some(0), None);
    assert_eq!(boundary.resume_after_seq, Some(0));
    assert_eq!(boundary.records, full.records);
    let inside = cut(&mut actor, Some(1), None);
    assert_eq!(inside.resume_after_seq, Some(1));
    assert_eq!(inside.base_seq, 3);
    assert_eq!(inside.records, full.records[1..]);
    let at_cut = cut(&mut actor, Some(3), None);
    assert_eq!(at_cut.resume_after_seq, Some(3));
    assert_eq!(at_cut.base_seq, 3);
    assert!(at_cut.records.is_empty());
    assert_eq!(
        cut(&mut actor, Some(4), None),
        full,
        "future cursor replaces"
    );
}

#[test]
fn evicted_cursor_replaces_but_the_first_retained_boundary_resumes() {
    let (mut actor, _facet, _handle) = engine();
    actor.ring = RecordRing::new(0);
    append(
        &mut actor,
        "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
    )
    .expect("accepted");
    let full = cut(&mut actor, None, None);
    assert_eq!(
        full.records.len(),
        1,
        "zero-byte budget keeps the newest record"
    );
    assert_eq!(full.dropped, 2);
    assert_eq!(cut(&mut actor, Some(1), None), full, "cursor was evicted");
    let boundary = cut(&mut actor, Some(2), None);
    assert_eq!(boundary.resume_after_seq, Some(2));
    assert_eq!(boundary.records, full.records);
    assert!(cut(&mut actor, Some(3), None).records.is_empty());
}

#[test]
fn an_empty_ring_resumes_only_at_its_cut() {
    let (mut actor, _facet, _handle) = engine();
    let empty = cut(&mut actor, Some(0), None);
    assert_eq!(empty.resume_after_seq, Some(0));
    assert!(empty.records.is_empty());
    assert_eq!(cut(&mut actor, Some(1), None).resume_after_seq, None);
    actor.core.seq = 9;
    assert_eq!(cut(&mut actor, Some(9), None).resume_after_seq, Some(9));
    assert_eq!(cut(&mut actor, Some(8), None).resume_after_seq, None);
    assert_eq!(cut(&mut actor, Some(10), None).resume_after_seq, None);
}

#[test]
fn upgrade_cuts_ignore_consumer_cursors_and_preserve_the_full_stream() {
    let (mut actor, _facet, _handle) = engine();
    append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
    let full = cut(&mut actor, None, None);
    let carried = cut(&mut actor, Some(2), Some(true));
    assert_eq!(carried, full, "sealing carries every retained record");
    assert!(matches!(
        append(&mut actor, "{\"type\":\"prompt\"}"),
        Err(AppendRejection::Overflow(_))
    ));
    assert_eq!(cut(&mut actor, Some(2), Some(false)), full);
    let mut restored = AgentSessionActor::restore(
        ResourceId::default(),
        "claude",
        None,
        CancellationToken::new(),
        4096,
        carried,
    )
    .actor;
    assert_eq!(cut(&mut restored, None, None), full);
    assert_eq!(
        append(&mut restored, "{\"type\":\"session_end\"}")
            .expect("accepted end")
            .first_seq,
        3
    );
    assert!(cut(&mut restored, None, None).ended);
}

#[test]
fn the_facet_resolves_and_carries_its_provenance() {
    let (_actor, facet, handle) = engine();
    assert_eq!(&*facet.provider, "claude");
    assert_eq!(facet.native_id.as_deref(), Some("abc"));
    assert_eq!(handle.kind, ResourceKind::AgentSession);
    assert!(handle.parent.is_some());
    assert!(
        handle.terminal().is_err(),
        "a session refuses the Terminal facet"
    );
    assert_eq!(
        handle.agent_session().expect("session facet").provider,
        facet.provider
    );
}

#[test]
fn appends_stamp_contiguous_sequences_and_one_timestamp() {
    let (mut actor, _facet, _handle) = engine();
    let first = append(&mut actor, "{\"type\":\"session_start\"}").expect("accepted");
    assert_eq!((first.first_seq, first.last_seq), (1, 1));
    let batch = append(
        &mut actor,
        "{\"type\":\"prompt\"}\n{\"type\":\"tool_start\"}\n",
    )
    .expect("accepted");
    assert_eq!((batch.first_seq, batch.last_seq), (2, 3));
    assert_eq!(batch.evidence, Some(StreamEvidence::Working));
    let bootstrap = actor.bootstrap(None);
    assert_eq!(bootstrap.base_seq, 3);
    assert_eq!(bootstrap.records.len(), 3);
    assert!(
        bootstrap.records[1].starts_with(b"{\"seq\":2,\"ts_ms\":"),
        "records replay with their stamped header"
    );
}

#[test]
fn an_invalid_record_commits_nothing() {
    let (mut actor, _facet, _handle) = engine();
    append(&mut actor, "{\"type\":\"prompt\"}").expect("accepted");
    let rejection = append(&mut actor, "{\"type\":\"stop\"}\n{\"type\":\"nope\"}")
        .expect_err("the batch is refused");
    assert!(matches!(rejection, AppendRejection::Invalid(_)));
    let bootstrap = actor.bootstrap(None);
    assert_eq!(bootstrap.base_seq, 1, "no sequence was consumed");
    assert_eq!(bootstrap.records.len(), 1);
}

#[test]
fn session_end_latches_the_stream_closed() {
    let (mut actor, _facet, _handle) = engine();
    let ended = append(&mut actor, "{\"type\":\"session_end\"}").expect("accepted");
    assert_eq!(ended.evidence, Some(StreamEvidence::Retract));
    assert_eq!(
        append(&mut actor, "{\"type\":\"prompt\"}"),
        Err(AppendRejection::Invalid(RecordError::AfterSessionEnd))
    );
}

#[test]
fn stamped_oversize_refuses_the_batch_before_any_mutation() {
    let (mut actor, _facet, handle) = engine();
    append(&mut actor, "{\"type\":\"prompt\"}").expect("seed");
    let before = actor.bootstrap(None);
    let mut subscriber = handle.output.subscribe();
    let prefix = "{\"type\":\"provider_raw\",\"data\":{\"text\":\"";
    let suffix = "\"}}";
    let oversized = format!(
        "{prefix}{}{suffix}",
        "x".repeat(record::MAX_RECORD_BYTES - 1 - prefix.len() - suffix.len())
    );
    assert_eq!(oversized.len() + 1, record::MAX_RECORD_BYTES);
    let batch = format!(
        "{{\"type\":\"ask\",\"data\":{{\"question\":\"continue?\"}}}}\n{oversized}\n{{\"type\":\"session_end\"}}"
    );
    assert!(matches!(
        append(&mut actor, &batch),
        Err(AppendRejection::Invalid(RecordError::Oversized { .. }))
    ));
    assert_eq!(
        actor.bootstrap(None),
        before,
        "ring, sequence and ended unchanged"
    );
    assert!(matches!(
        subscriber.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let accepted = append(&mut actor, "{\"type\":\"stop\"}").expect("still open");
    assert_eq!((accepted.first_seq, accepted.last_seq), (2, 2));
}

#[test]
fn overflow_evicts_and_the_bootstrap_reports_the_toll() {
    let bundle = AgentSessionActor::build(
        ResourceId::default(),
        "claude",
        None,
        CancellationToken::new(),
        120,
    );
    let mut actor = bundle.actor;
    for _ in 0..8 {
        append(&mut actor, "{\"type\":\"tool_end\"}").expect("accepted");
    }
    let bootstrap = actor.bootstrap(None);
    assert_eq!(bootstrap.base_seq, 8, "every record still took a sequence");
    assert!(bootstrap.dropped > 0, "the ring evicted its oldest records");
    assert_eq!(
        bootstrap.records.len() as u64 + bootstrap.dropped,
        8,
        "retained plus evicted accounts for every record"
    );
}

#[test]
fn a_restored_engine_continues_the_carried_stream() {
    let (mut old, _facet, _handle) = engine();
    append(&mut old, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
    let mut carried = old.bootstrap(None);
    carried.dropped = 5;
    let bundle = AgentSessionActor::restore(
        ResourceId::default(),
        "claude",
        Some("abc"),
        CancellationToken::new(),
        4096,
        carried.clone(),
    );
    let mut actor = bundle.actor;
    assert_eq!(actor.bootstrap(None), carried, "the cut replays unchanged");
    let next = append(&mut actor, "{\"type\":\"session_end\"}").expect("accepted");
    assert_eq!(next.first_seq, 3, "the sequence continues");

    let ended = actor.bootstrap(None);
    assert!(ended.ended);
    let bundle = AgentSessionActor::restore(
        ResourceId::default(),
        "claude",
        None,
        CancellationToken::new(),
        4096,
        ended,
    );
    let mut actor = bundle.actor;
    assert_eq!(
        append(&mut actor, "{\"type\":\"prompt\"}"),
        Err(AppendRejection::Invalid(RecordError::AfterSessionEnd)),
        "an ended stream stays ended"
    );
}

#[test]
fn a_sealed_stream_refuses_appends_until_unsealed() {
    let (mut actor, _facet, _handle) = engine();
    append(&mut actor, "{\"type\":\"prompt\"}").expect("accepted");
    let (reply, mut rx) = oneshot::channel();
    actor.handle_bootstrap(BootstrapRequest {
        reply,
        seal: Some(true),
        after_seq: None,
    });
    assert_eq!(rx.try_recv().expect("the cut").base_seq, 1);
    assert!(matches!(
        append(&mut actor, "{\"type\":\"stop\"}"),
        Err(AppendRejection::Overflow(_))
    ));
    actor.handle_bootstrap(BootstrapRequest {
        reply: oneshot::channel().0,
        seal: Some(false),
        after_seq: None,
    });
    let next = append(&mut actor, "{\"type\":\"stop\"}").expect("unsealed");
    assert_eq!(next.first_seq, 2, "the refused append consumed nothing");
}

/// `AgentSessionSeal::drop` cannot await the actor: all it can do is queue
/// the unseal. An append queued behind that unseal in the same wake-up must
/// still find an unsealed stream, so `run` has to poll the bootstrap
/// mailbox before the append mailbox. Regression for #1035.
#[tokio::test(flavor = "current_thread")]
async fn a_queued_unseal_is_applied_before_the_append_queued_behind_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = AgentSessionActor::build(
                ResourceId::default(),
                "claude",
                Some("abc"),
                CancellationToken::new(),
                4096,
            );
            let ResourceFacetHandle::AgentSession(facet) = &bundle.handle.facet else {
                panic!("built the wrong facet");
            };
            let bootstrap = facet.bootstrap.clone();
            let append_tx = facet.append.clone();
            tokio::task::spawn_local(bundle.actor.run());

            // Prove the actor is polling, then leave it sealed.
            let (reply, cut) = oneshot::channel();
            bootstrap
                .send(BootstrapRequest {
                    reply,
                    seal: Some(true),
                    after_seq: None,
                })
                .await
                .expect("the actor is running");
            assert_eq!(cut.await.expect("the cut").base_seq, 0);

            // Queue the unseal and an append without yielding, so the actor
            // wakes with both mailboxes ready at once.
            bootstrap
                .try_send(BootstrapRequest {
                    reply: oneshot::channel().0,
                    seal: Some(false),
                    after_seq: None,
                })
                .expect("the bootstrap mailbox has room");
            let (reply, accepted) = oneshot::channel();
            append_tx
                .try_send(AppendRequest {
                    bytes: Bytes::copy_from_slice(b"{\"type\":\"stop\"}"),
                    reply,
                    publication: None,
                })
                .expect("the append mailbox has room");

            let verdict = accepted.await.expect("the actor always replies");
            assert!(
                verdict.is_ok(),
                "an append queued behind its own unseal saw the seal: {verdict:?}"
            );
        })
        .await;
}

#[test]
fn live_output_carries_whole_records_sequenced_by_the_last() {
    let (mut actor, _facet, handle) = engine();
    let mut subscriber = handle.output.subscribe();
    let accepted =
        append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
    match subscriber.try_recv().expect("one live frame") {
        PaneOutput::Live { seq, bytes, .. } => {
            assert_eq!(seq, accepted.last_seq);
            let text = String::from_utf8(bytes.to_vec()).expect("utf-8");
            assert_eq!(text.lines().count(), 2);
            assert!(text.ends_with('\n'), "every record ends its line");
        }
        other => panic!("expected live output, got {other:?}"),
    }
}
