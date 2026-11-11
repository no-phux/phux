//! `AgentSession` resume: an `AgentEventsJsonlResumeV1` cut appends to the
//! committed log only from its exact anchor, and every refusal keeps the
//! committed log, derived state and cursor.

use super::*;

#[test]
fn resumed_agent_bootstrap_appends_atomically_and_preserves_derived_state() {
    let mut kernel = published_agent_for_batches(0);
    let agent = terminal(2);
    let mut effects = EffectBuffer::new();
    let identity = agent_line(
        1,
        "session_start",
        r#"{"provider":"omp","native_id":"committed-native-id"}"#,
    );
    let initial = identity + &agent_line(2, "ask", "{}");
    kernel
        .update(
            output(&agent, stream(3), bootstrap(4), 2, initial.as_bytes()),
            &mut effects,
        )
        .unwrap();
    let committed = kernel.agent_stream_info(&agent).unwrap();
    assert_eq!(committed.last_seq, 2);
    agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 2, 4, &mut effects).unwrap();
    for (chunk_seq, seq) in [(0, 3), (1, 4)] {
        kernel
            .update(
                chunk(
                    &agent,
                    stream(5),
                    bootstrap(6),
                    chunk_seq,
                    agent_line(seq, "provider_raw", "{}").as_bytes(),
                ),
                &mut effects,
            )
            .unwrap();
        assert!(effects.is_empty());
        assert_eq!(kernel.agent_stream_info(&agent), Some(committed));
        assert_eq!(agent_sequences(&kernel), [1, 2]);
        assert_eq!(
            kernel.agent_session(&agent).unwrap().state.status,
            AgentSessionStatus::Blocked
        );
    }
    kernel
        .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
        .unwrap();
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![3, 4],
            AgentRecordMode::Append,
            committed_agent(stream(5), bootstrap(6), 4),
        ))
    );
    assert_eq!(agent_sequences(&kernel), [1, 2, 3, 4]);
    let view = kernel.agent_session(&agent).unwrap();
    assert_eq!(view.state.provider.as_deref(), Some("omp"));
    assert_eq!(view.state.native_id.as_deref(), Some("committed-native-id"));
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    assert_eq!(kernel.agent_stream_info(&agent).unwrap().last_seq, 4);
    assert!(matches!(
        kernel.update(
            output(
                &agent,
                stream(3),
                bootstrap(4),
                3,
                agent_line(3, "stop", "{}").as_bytes()
            ),
            &mut effects,
        ),
        Err(KernelError::RetiredGeneration { .. })
    ));
    kernel
        .update(
            output(
                &agent,
                stream(5),
                bootstrap(6),
                5,
                agent_line(5, "stop", "{}").as_bytes(),
            ),
            &mut effects,
        )
        .unwrap();
    assert_eq!(agent_sequences(&kernel), [1, 2, 3, 4, 5]);
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![5],
            AgentRecordMode::Append,
            committed_agent(stream(5), bootstrap(6), 5),
        ))
    );
    assert_eq!(
        kernel.agent_session(&agent).unwrap().state.status,
        AgentSessionStatus::Done
    );
}

#[test]
fn empty_agent_resume_publishes_without_clearing_evidence() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "ask")]).unwrap();
    agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 1, 1, &mut effects).unwrap();
    kernel
        .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
        .unwrap();
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![],
            AgentRecordMode::Append,
            committed_agent(stream(5), bootstrap(6), 1),
        )),
        "an empty resume publishes the new generation without clearing evidence"
    );
    assert_eq!(agent_sequences(&kernel), [1]);
    assert_eq!(
        kernel.agent_stream_info(&agent).unwrap().bootstrap_id,
        bootstrap(6)
    );
    assert_eq!(
        kernel.agent_session(&agent).unwrap().state.status,
        AgentSessionStatus::Blocked
    );
    agent_begin(
        &mut kernel,
        &agent,
        stream(7),
        bootstrap(8),
        1,
        &mut effects,
    )
    .unwrap();
    kernel
        .update(ready(&agent, stream(7), bootstrap(8), None), &mut effects)
        .unwrap();
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![],
            AgentRecordMode::Replace,
            committed_agent(stream(7), bootstrap(8), 1),
        ))
    );
    assert!(agent_sequences(&kernel).is_empty());
    assert_eq!(
        kernel.agent_session(&agent).unwrap().state.status,
        AgentSessionStatus::Working
    );
}

#[test]
fn agent_eviction_fallback_replaces_the_log_and_rebuilds_state() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    apply_agent_batch(&mut kernel, &mut effects, 2, &[(1, "prompt"), (2, "ask")]).unwrap();
    agent_begin(
        &mut kernel,
        &agent,
        stream(5),
        bootstrap(6),
        6,
        &mut effects,
    )
    .unwrap();
    kernel
        .update(
            chunk(
                &agent,
                stream(5),
                bootstrap(6),
                0,
                batch_payload(&[(5, "prompt"), (6, "stop")]).as_bytes(),
            ),
            &mut effects,
        )
        .unwrap();
    assert_eq!(agent_sequences(&kernel), [1, 2]);
    kernel
        .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
        .unwrap();
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![5, 6],
            AgentRecordMode::Replace,
            committed_agent(stream(5), bootstrap(6), 6),
        ))
    );
    assert_eq!(agent_sequences(&kernel), [5, 6]);
    assert_eq!(
        kernel.agent_session(&agent).unwrap().state.status,
        AgentSessionStatus::Done
    );
    assert_eq!(kernel.agent_stream_info(&agent).unwrap().last_seq, 6);
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
        let mut kernel = published_agent_for_batches(0);
        let mut effects = EffectBuffer::new();
        let agent = terminal(2);
        apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "ask")]).unwrap();
        let committed = kernel.agent_stream_info(&agent).unwrap();
        agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 1, 3, &mut effects).unwrap();
        let chunk_result = kernel.update(
            chunk(&agent, stream(5), bootstrap(6), 0, payload.as_bytes()),
            &mut effects,
        );
        assert!(agent_publication(&effects).is_none());
        let result = kernel.update(ready(&agent, stream(5), bootstrap(6), None), &mut effects);
        assert!(
            result.is_err(),
            "invalid delta {payload:?}: chunk={chunk_result:?}"
        );
        assert!(agent_publication(&effects).is_none());
        assert_eq!(kernel.agent_stream_info(&agent), Some(committed));
        assert_eq!(agent_sequences(&kernel), [1]);
        assert_eq!(
            kernel.agent_session(&agent).unwrap().state.status,
            AgentSessionStatus::Blocked
        );
        apply_agent_batch(&mut kernel, &mut effects, 2, &[(2, "stop")]).unwrap();
        assert_eq!(kernel.agent_stream_info(&agent).unwrap().last_seq, 2);
    }
}

#[test]
fn agent_resume_requires_an_exact_committed_anchor_and_non_decreasing_cut() {
    let mut kernel = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    assert!(matches!(
        agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 0, 0, &mut effects),
        Err(KernelError::AgentResumeUnavailable(_))
    ));
    assert!(kernel.agent_stream_info(&terminal(2)).is_none());
    let mut kernel = published_agent_for_batches(0);
    apply_agent_batch(&mut kernel, &mut effects, 2, &[(1, "prompt"), (2, "ask")]).unwrap();
    let committed = kernel.agent_stream_info(&terminal(2));
    for (after_seq, base_seq) in [(1, 2), (3, 3), (2, 1)] {
        assert!(
            agent_resume_begin(
                &mut kernel,
                (stream(5), bootstrap(6)),
                after_seq,
                base_seq,
                &mut effects
            )
            .is_err()
        );
        assert_eq!(kernel.agent_stream_info(&terminal(2)), committed);
        assert_eq!(agent_sequences(&kernel), [1, 2]);
    }
}

#[test]
fn gapped_agent_bootstrap_chunks_cannot_later_publish_a_partial_delta() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    let committed = kernel.agent_stream_info(&agent);
    agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 0, 1, &mut effects).unwrap();
    assert!(matches!(
        kernel.update(
            chunk(
                &agent,
                stream(5),
                bootstrap(6),
                1,
                agent_line(1, "ask", "{}").as_bytes()
            ),
            &mut effects,
        ),
        Err(KernelError::ChunkGap { .. })
    ));
    assert!(matches!(
        kernel.update(ready(&agent, stream(5), bootstrap(6), None), &mut effects),
        Err(KernelError::RetiredGeneration { .. })
    ));
    assert_eq!(kernel.agent_stream_info(&agent), committed);
    assert!(agent_sequences(&kernel).is_empty());
}

#[test]
fn agent_resume_rechecks_its_anchor_at_ready() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 0, 1, &mut effects).unwrap();
    kernel
        .update(
            chunk(
                &agent,
                stream(5),
                bootstrap(6),
                0,
                agent_line(1, "ask", "{}").as_bytes(),
            ),
            &mut effects,
        )
        .unwrap();
    apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "stop")]).unwrap();
    let committed = kernel.agent_stream_info(&agent);
    assert!(
        kernel
            .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
            .is_err()
    );
    assert!(agent_publication(&effects).is_none());
    assert_eq!(agent_sequences(&kernel), [1]);
    assert_eq!(kernel.agent_stream_info(&agent), committed);
    assert_eq!(
        kernel.agent_session(&agent).unwrap().state.status,
        AgentSessionStatus::Done
    );
}

#[test]
fn agent_cursor_is_unavailable_before_ready_and_cleared_by_resource_lifecycle() {
    let mut staged = kernel(ReadyMode::ChunkFirst);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    agent_begin(
        &mut staged,
        &agent,
        stream(3),
        bootstrap(4),
        1,
        &mut effects,
    )
    .unwrap();
    staged
        .update(
            chunk(
                &agent,
                stream(3),
                bootstrap(4),
                0,
                agent_line(1, "ask", "{}").as_bytes(),
            ),
            &mut effects,
        )
        .unwrap();
    assert!(staged.agent_stream_info(&agent).is_none());
    for transition in 0..3 {
        let mut kernel = published_agent_for_batches(0);
        assert!(kernel.agent_stream_info(&agent).is_some());
        match transition {
            0 => {
                kernel.update(closed(&agent), &mut effects).unwrap();
            }
            1 => {
                assert!(kernel.detach_terminal(&agent));
            }
            _ => kernel.release_incarnation(),
        }
        assert!(kernel.agent_stream_info(&agent).is_none());
        assert!(kernel.agent_session(&agent).is_none());
    }
}

#[test]
fn agent_resume_at_sequence_exhaustion_accepts_only_the_empty_suffix() {
    let mut kernel = published_agent_for_batches(u64::MAX);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    agent_resume_begin(
        &mut kernel,
        (stream(5), bootstrap(6)),
        u64::MAX,
        u64::MAX,
        &mut effects,
    )
    .unwrap();
    kernel
        .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
        .unwrap();
    assert_eq!(
        agent_publication(&effects),
        Some((
            agent.clone(),
            vec![],
            AgentRecordMode::Append,
            committed_agent(stream(5), bootstrap(6), u64::MAX),
        )),
        "an empty suffix still publishes the advanced generation"
    );
    assert_eq!(kernel.agent_stream_info(&agent).unwrap().last_seq, u64::MAX);
    assert!(matches!(
        kernel.update(
            output(&agent, stream(5), bootstrap(6), 0, b""),
            &mut effects
        ),
        Err(KernelError::SequenceExhausted)
    ));
}

#[test]
fn full_agent_bootstrap_also_validates_continuity_and_the_cut_before_replacement() {
    for records in [
        vec![(7, "stop"), (9, "prompt")],
        vec![(7, "stop"), (7, "ask"), (8, "prompt"), (9, "stop")],
        vec![(7, "stop"), (8, "prompt")],
        vec![(9, "stop"), (10, "prompt")],
    ] {
        let mut kernel = published_agent_for_batches(0);
        let mut effects = EffectBuffer::new();
        let agent = terminal(2);
        apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "ask")]).unwrap();
        let committed = kernel.agent_stream_info(&agent);
        agent_begin(
            &mut kernel,
            &agent,
            stream(5),
            bootstrap(6),
            9,
            &mut effects,
        )
        .unwrap();
        kernel
            .update(
                chunk(
                    &agent,
                    stream(5),
                    bootstrap(6),
                    0,
                    batch_payload(&records).as_bytes(),
                ),
                &mut effects,
            )
            .unwrap();
        assert!(
            kernel
                .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
                .is_err()
        );
        assert!(agent_publication(&effects).is_none());
        assert_eq!(agent_sequences(&kernel), [1]);
        assert_eq!(kernel.agent_stream_info(&agent), committed);
        assert_eq!(
            kernel.agent_session(&agent).unwrap().state.status,
            AgentSessionStatus::Blocked
        );
    }
}

#[test]
fn large_agent_resume_preserves_state_while_bounding_the_retained_log() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "ask")]).unwrap();
    let base_seq = super::super::agent_stream::DEFAULT_LOG_CAPACITY as u64 + 2;
    agent_resume_begin(
        &mut kernel,
        (stream(5), bootstrap(6)),
        1,
        base_seq,
        &mut effects,
    )
    .unwrap();
    let records: Vec<_> = (2..=base_seq).map(|seq| (seq, "provider_raw")).collect();
    kernel
        .update(
            chunk(
                &agent,
                stream(5),
                bootstrap(6),
                0,
                batch_payload(&records).as_bytes(),
            ),
            &mut effects,
        )
        .unwrap();
    kernel
        .update(ready(&agent, stream(5), bootstrap(6), None), &mut effects)
        .unwrap();
    let view = kernel.agent_session(&agent).unwrap();
    assert_eq!(
        view.log.len(),
        super::super::agent_stream::DEFAULT_LOG_CAPACITY
    );
    assert_eq!(view.log.records().next().unwrap().seq, 3);
    assert_eq!(view.log.last().unwrap().seq, base_seq);
    assert_eq!(view.state.status, AgentSessionStatus::Blocked);
    let publication = agent_publication(&effects).expect("resume publication");
    assert_eq!(publication.1.len(), records.len());
    assert_eq!(publication.2, AgentRecordMode::Append);
    assert_eq!(
        publication.3,
        committed_agent(stream(5), bootstrap(6), base_seq)
    );
}

#[test]
fn agent_resume_staging_obeys_the_existing_chunk_ceiling() {
    let mut kernel = published_agent_for_batches(0);
    let mut effects = EffectBuffer::new();
    let agent = terminal(2);
    let committed = kernel.agent_stream_info(&agent);
    agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 0, 0, &mut effects).unwrap();
    for chunk_seq in 0..u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap() {
        kernel
            .update(
                chunk(&agent, stream(5), bootstrap(6), chunk_seq, b""),
                &mut effects,
            )
            .unwrap();
    }
    assert!(matches!(
        kernel.update(
            chunk(
                &agent,
                stream(5),
                bootstrap(6),
                u32::try_from(MAX_BOOTSTRAP_STAGING_CHUNKS).unwrap(),
                b""
            ),
            &mut effects
        ),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));
    assert_eq!(kernel.agent_stream_info(&agent), committed);
    assert!(agent_publication(&effects).is_none());
}

#[test]
fn expired_agent_resume_staging_preserves_the_committed_generation() {
    for transition in 0..3 {
        let mut kernel = published_agent_for_batches(0);
        let mut effects = EffectBuffer::new();
        let agent = terminal(2);
        apply_agent_batch(&mut kernel, &mut effects, 1, &[(1, "ask")]).unwrap();
        let committed = kernel.agent_stream_info(&agent);
        assert_eq!(kernel.expire_bootstrap_staging(0, &mut effects), 0);
        agent_resume_begin(&mut kernel, (stream(5), bootstrap(6)), 1, 1, &mut effects).unwrap();
        assert_eq!(
            kernel.expire_bootstrap_staging(MAX_BOOTSTRAP_STAGING_IDLE_MS, &mut effects),
            0
        );
        let expired = MAX_BOOTSTRAP_STAGING_IDLE_MS + 1;
        match transition {
            0 => assert!(matches!(
                kernel.update_at(
                    expired,
                    chunk(&agent, stream(5), bootstrap(6), 0, b""),
                    &mut effects
                ),
                Err(KernelError::BootstrapStagingLimitExceeded)
            )),
            1 => assert!(matches!(
                kernel.update_at(
                    expired,
                    ready(&agent, stream(5), bootstrap(6), None),
                    &mut effects
                ),
                Err(KernelError::BootstrapStagingLimitExceeded)
            )),
            _ => assert_eq!(kernel.expire_bootstrap_staging(expired, &mut effects), 1),
        }
        assert_eq!(kernel.agent_stream_info(&agent), committed);
        assert_eq!(agent_sequences(&kernel), [1]);
        assert_eq!(
            kernel.agent_session(&agent).unwrap().state.status,
            AgentSessionStatus::Blocked
        );
        assert!(agent_publication(&effects).is_none());
    }
}
