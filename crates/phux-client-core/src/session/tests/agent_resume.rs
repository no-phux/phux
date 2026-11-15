//! `AgentSession` resume: an `AgentEventsJsonlResumeV1` cut appends to the
//! committed log only from its exact anchor, and every refusal keeps the
//! committed log, derived state and cursor.

use super::*;

/// The generation every resume here stages.
fn resumed() -> Gen {
    agent_gen(5, 6)
}

#[test]
fn resumed_agent_bootstrap_appends_atomically_and_preserves_derived_state() {
    let mut rig = agent_rig(0);
    let (live, next) = (agent_gen(3, 4), resumed());
    let identity = agent_line(
        1,
        "session_start",
        r#"{"provider":"omp","native_id":"committed-native-id"}"#,
    );
    let initial = identity + &agent_line(2, "ask", "{}");
    rig.ok(live.output(2, initial.as_bytes()));
    let committed = rig.cursor().unwrap();
    assert_eq!(committed.last_seq, 2);
    rig.ok(next.resume(2, 4));
    for (chunk_seq, seq) in [(0, 3), (1, 4)] {
        rig.ok(next.chunk(chunk_seq, agent_line(seq, "provider_raw", "{}").as_bytes()));
        assert!(rig.fx.is_empty());
        assert_eq!(rig.cursor(), Some(committed));
        assert_eq!(rig.seqs(), [1, 2]);
        assert_eq!(rig.status(), AgentSessionStatus::Blocked);
    }
    rig.ok(next.ready(None));
    assert_eq!(
        rig.publication(),
        published(&next, &[3, 4], AgentRecordMode::Append, 4)
    );
    assert_eq!(rig.seqs(), [1, 2, 3, 4]);
    let view = rig.k.agent_session(&next.id).unwrap();
    assert_eq!(view.state.provider.as_deref(), Some("omp"));
    assert_eq!(view.state.native_id.as_deref(), Some("committed-native-id"));
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    assert_eq!(rig.cursor().unwrap().last_seq, 4);
    assert!(matches!(
        rig.up(live.output(3, agent_line(3, "stop", "{}").as_bytes())),
        Err(KernelError::RetiredGeneration { .. })
    ));
    rig.ok(next.output(5, agent_line(5, "stop", "{}").as_bytes()));
    assert_eq!(rig.seqs(), [1, 2, 3, 4, 5]);
    assert_eq!(
        rig.publication(),
        published(&next, &[5], AgentRecordMode::Append, 5)
    );
    assert_eq!(rig.status(), AgentSessionStatus::Done);
}

#[test]
fn empty_agent_resume_publishes_without_clearing_evidence() {
    let mut rig = agent_rig(0);
    let next = resumed();
    rig.batch(1, &[(1, "ask")]).unwrap();
    rig.ok(next.resume(1, 1));
    rig.ok(next.ready(None));
    assert_eq!(
        rig.publication(),
        published(&next, &[], AgentRecordMode::Append, 1),
        "an empty resume publishes the new generation without clearing evidence"
    );
    assert_eq!(rig.seqs(), [1]);
    assert_eq!(rig.cursor().unwrap().bootstrap_id, next.b);
    assert_eq!(rig.status(), AgentSessionStatus::Blocked);
    let full = agent_gen(7, 8);
    rig.ok(full.agent_begin(1));
    rig.ok(full.ready(None));
    assert_eq!(
        rig.publication(),
        published(&full, &[], AgentRecordMode::Replace, 1)
    );
    assert!(rig.seqs().is_empty());
    assert_eq!(rig.status(), AgentSessionStatus::Working);
}

#[test]
fn agent_eviction_fallback_replaces_the_log_and_rebuilds_state() {
    let mut rig = agent_rig(0);
    let next = resumed();
    rig.batch(2, &[(1, "prompt"), (2, "ask")]).unwrap();
    rig.ok(next.agent_begin(6));
    rig.ok(next.chunk(0, batch_payload(&[(5, "prompt"), (6, "stop")]).as_bytes()));
    assert_eq!(rig.seqs(), [1, 2]);
    rig.ok(next.ready(None));
    assert_eq!(
        rig.publication(),
        published(&next, &[5, 6], AgentRecordMode::Replace, 6)
    );
    assert_eq!(rig.seqs(), [5, 6]);
    assert_eq!(rig.status(), AgentSessionStatus::Done);
    assert_eq!(rig.cursor().unwrap().last_seq, 6);
}

/// Stage `begin` on [`resumed`] after committing record 1 ("ask"), feed
/// `payload`, and require READY to refuse it without touching the commit.
fn assert_bad_delta_keeps_the_commit(payload: &str, begin: fn(&Gen) -> KernelInput<'_>) {
    let mut rig = agent_rig(0);
    let next = resumed();
    rig.batch(1, &[(1, "ask")]).unwrap();
    let committed = rig.cursor();
    rig.ok(begin(&next));
    let chunk_result = rig.up(next.chunk(0, payload.as_bytes()));
    assert!(rig.publication().is_none());
    let result = rig.up(next.ready(None));
    assert!(
        result.is_err(),
        "invalid delta {payload:?}: chunk={chunk_result:?}"
    );
    assert!(rig.publication().is_none());
    assert_eq!(rig.cursor(), committed);
    assert_eq!(rig.seqs(), [1]);
    assert_eq!(rig.status(), AgentSessionStatus::Blocked);
    // The commit keeps accepting live records.
    rig.batch(2, &[(2, "stop")]).unwrap();
    assert_eq!(rig.cursor().unwrap().last_seq, 2);
}

#[test]
fn invalid_agent_resume_records_preserve_the_committed_log_state_and_cursor() {
    for payload in [
        batch_payload(&[(2, "stop"), (4, "prompt")]),
        batch_payload(&[(2, "stop"), (2, "prompt"), (3, "ask")]),
        batch_payload(&[(3, "stop")]),
        batch_payload(&[(2, "stop")]),
        batch_payload(&[(2, "stop"), (3, "stop"), (4, "ask")]),
        String::new(),
        String::from("{malformed}\n"),
        batch_payload(&[(2, "stop")]) + "{\"seq\":3,\"ts_ms\":1}\n",
    ] {
        assert_bad_delta_keeps_the_commit(&payload, |g| g.resume(1, 3));
    }
}

#[test]
fn full_agent_bootstrap_also_validates_continuity_and_the_cut_before_replacement() {
    for records in [
        vec![(7, "stop"), (9, "prompt")],
        vec![(7, "stop"), (7, "ask"), (8, "prompt"), (9, "stop")],
        vec![(7, "stop"), (8, "prompt")],
        vec![(9, "stop"), (10, "prompt")],
    ] {
        assert_bad_delta_keeps_the_commit(&batch_payload(&records), |g| g.agent_begin(9));
    }
}

#[test]
fn agent_resume_requires_an_exact_committed_anchor_and_non_decreasing_cut() {
    let mut rig = Rig::new(ReadyMode::ChunkFirst);
    assert!(matches!(
        rig.up(resumed().resume(0, 0)),
        Err(KernelError::AgentResumeUnavailable(_))
    ));
    assert!(rig.cursor().is_none());
    let mut rig = agent_rig(0);
    rig.batch(2, &[(1, "prompt"), (2, "ask")]).unwrap();
    let committed = rig.cursor();
    for (after_seq, base_seq) in [(1, 2), (3, 3), (2, 1)] {
        assert!(rig.up(resumed().resume(after_seq, base_seq)).is_err());
        assert_eq!(rig.cursor(), committed);
        assert_eq!(rig.seqs(), [1, 2]);
    }
}

#[test]
fn gapped_agent_bootstrap_chunks_cannot_later_publish_a_partial_delta() {
    let mut rig = agent_rig(0);
    let next = resumed();
    let committed = rig.cursor();
    rig.ok(next.resume(0, 1));
    assert!(matches!(
        rig.up(next.chunk(1, agent_line(1, "ask", "{}").as_bytes())),
        Err(KernelError::ChunkGap { .. })
    ));
    assert!(matches!(
        rig.up(next.ready(None)),
        Err(KernelError::RetiredGeneration { .. })
    ));
    assert_eq!(rig.cursor(), committed);
    assert!(rig.seqs().is_empty());
}

#[test]
fn agent_resume_rechecks_its_anchor_at_ready() {
    let mut rig = agent_rig(0);
    let next = resumed();
    rig.ok(next.resume(0, 1));
    rig.ok(next.chunk(0, agent_line(1, "ask", "{}").as_bytes()));
    rig.batch(1, &[(1, "stop")]).unwrap();
    let committed = rig.cursor();
    assert!(rig.up(next.ready(None)).is_err());
    assert!(rig.publication().is_none());
    assert_eq!(rig.seqs(), [1]);
    assert_eq!(rig.cursor(), committed);
    assert_eq!(rig.status(), AgentSessionStatus::Done);
}

#[test]
fn agent_cursor_is_unavailable_before_ready_and_cleared_by_resource_lifecycle() {
    let mut staged = Rig::new(ReadyMode::ChunkFirst);
    let g = agent_gen(3, 4);
    staged.ok(g.agent_begin(1));
    staged.ok(g.chunk(0, agent_line(1, "ask", "{}").as_bytes()));
    assert!(staged.cursor().is_none());
    for transition in 0..3 {
        let mut rig = agent_rig(0);
        assert!(rig.cursor().is_some());
        match transition {
            0 => rig.ok(closed(&g.id)),
            1 => assert!(rig.k.detach_terminal(&g.id)),
            _ => rig.k.release_incarnation(),
        }
        assert!(rig.cursor().is_none());
        assert!(rig.k.agent_session(&g.id).is_none());
    }
}

#[test]
fn agent_resume_at_sequence_exhaustion_accepts_only_the_empty_suffix() {
    let mut rig = agent_rig(u64::MAX);
    let next = resumed();
    rig.ok(next.resume(u64::MAX, u64::MAX));
    rig.ok(next.ready(None));
    assert_eq!(
        rig.publication(),
        published(&next, &[], AgentRecordMode::Append, u64::MAX),
        "an empty suffix still publishes the advanced generation"
    );
    assert_eq!(rig.cursor().unwrap().last_seq, u64::MAX);
    assert!(matches!(
        rig.up(next.output(0, b"")),
        Err(KernelError::SequenceExhausted)
    ));
}

#[test]
fn large_agent_resume_preserves_state_while_bounding_the_retained_log() {
    let capacity = super::super::agent_stream::DEFAULT_LOG_CAPACITY;
    let mut rig = agent_rig(0);
    let next = resumed();
    rig.batch(1, &[(1, "ask")]).unwrap();
    let base_seq = capacity as u64 + 2;
    rig.ok(next.resume(1, base_seq));
    let records: Vec<_> = (2..=base_seq).map(|seq| (seq, "provider_raw")).collect();
    rig.ok(next.chunk(0, batch_payload(&records).as_bytes()));
    rig.ok(next.ready(None));
    let view = rig.k.agent_session(&next.id).unwrap();
    assert_eq!(view.log.len(), capacity);
    assert_eq!(view.log.records().next().unwrap().seq, 3);
    assert_eq!(view.log.last().unwrap().seq, base_seq);
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    let publication = rig.publication().expect("resume publication");
    assert_eq!(publication.1.len(), records.len());
    assert_eq!(publication.2, AgentRecordMode::Append);
    assert_eq!(publication.3, committed_agent(next.s, next.b, base_seq));
}

#[test]
fn agent_resume_staging_obeys_the_existing_chunk_ceiling() {
    let mut rig = agent_rig(0);
    let next = resumed();
    let committed = rig.cursor();
    rig.ok(next.resume(0, 0));
    let limit = u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap();
    for chunk_seq in 0..limit {
        rig.ok(next.chunk(chunk_seq, b""));
    }
    assert!(matches!(
        rig.up(next.chunk(limit, b"")),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert_eq!(rig.cursor(), committed);
    assert!(rig.publication().is_none());
}

#[test]
fn expired_agent_resume_staging_preserves_the_committed_generation() {
    for transition in 0..3 {
        let mut rig = agent_rig(0);
        let next = resumed();
        rig.batch(1, &[(1, "ask")]).unwrap();
        let committed = rig.cursor();
        assert_eq!(rig.k.expire_bootstrap_staging(0, &mut rig.fx), 0);
        rig.ok(next.resume(1, 1));
        let idle = MAX_BOOTSTRAP_STAGING_IDLE_MS;
        assert_eq!(rig.k.expire_bootstrap_staging(idle, &mut rig.fx), 0);
        let expired = idle + 1;
        let input = match transition {
            0 => Some(next.chunk(0, b"")),
            1 => Some(next.ready(None)),
            _ => None,
        };
        if let Some(input) = input {
            assert!(matches!(
                rig.k.update_at(expired, input, &mut rig.fx),
                Err(KernelError::BootstrapStagingLimitExceeded)
            ));
        } else {
            assert_eq!(rig.k.expire_bootstrap_staging(expired, &mut rig.fx), 1);
        }
        assert_eq!(rig.cursor(), committed);
        assert_eq!(rig.seqs(), [1]);
        assert_eq!(rig.status(), AgentSessionStatus::Blocked);
        assert!(rig.publication().is_none());
    }
}
