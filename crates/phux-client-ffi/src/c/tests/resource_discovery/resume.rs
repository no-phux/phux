//! Agent bootstrap fixtures and unaccepted full-cut replacement delivery.
use super::*;

pub(super) fn agent_bootstrap_frames(
    generation: u64,
    profile: phux_protocol::BootstrapStreamProfile,
    base_seq: u64,
    payload: String,
) -> Vec<FrameKind> {
    let terminal_id = ResourceId::local(MIXED_AGENT);
    let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).unwrap();
    let bootstrap_id = phux_protocol::BootstrapId::new(generation).unwrap();
    let mut frames = vec![FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile,
        cols: 0,
        rows: 0,
        base_seq,
    }];
    if !payload.is_empty() {
        frames.push(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: payload.into(),
        });
    }
    frames.push(FrameKind::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    frames
}

pub(super) fn feed_agent_bootstrap(client: *mut PhuxClient, frames: Vec<FrameKind>) {
    for frame in frames {
        assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
    }
}

#[test]
fn c_full_agent_cut_supersedes_uncleared_effects_without_accepting_its_resume() {
    use phux_protocol::BootstrapStreamProfile::{AgentEventsJsonlResumeV1, AgentEventsJsonlV1};
    let client = attached_resume_client();
    open_agent_stream(client);
    // The original retained prefix and its live append have already crossed
    // leased runtime drains, but the host has not called effect_clear.
    feed_agent_bootstrap(
        client,
        agent_bootstrap_frames(
            6,
            AgentEventsJsonlV1,
            3,
            record(2, "ask", "{}") + &record(3, "tool_start", "{}"),
        ),
    );
    // SAFETY: the fixture owns this client and reads borrowed effects only.
    let staged = (0..unsafe { phux_client_effect_count(client) })
        .map(|index| effect_at(client, index))
        .filter(|effect| effect.kind == EFFECT_AGENT_RECORDS)
        .map(|effect| (effect.detail, effect.bootstrap_id, effect.seq))
        .collect::<Vec<_>>();
    assert_eq!(staged, [(AGENT_RECORDS_RETAINED, 6, 3)]);
    // Supersession is not acceptance. Another transport replacement before
    // effect_clear must still replay this full cut, not the original prefix.
    // SAFETY: this fixture exclusively owns the live client.
    unsafe {
        (*client).inner.control().connection_opened();
        (*client).inner.process_runtime_events().unwrap();
    }
    let caps =
        test_support::caps(&[]).with_features_ext(phux_protocol::caps::ServerFeatureExtSet::with(
            &[phux_protocol::caps::ServerFeatureExt::AgentStreamResume],
        ));
    let hello = test_support::hello_ok(caps, phux_protocol::BootstrapProfile::SynthesizedVtRaw);
    assert_eq!(feed(client, &hello), PhuxClientResult::Ok);
    let _ = outgoing(client);
    test_support::queue_attach(client, 7);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot(true))),
        PhuxClientResult::Ok
    );
    feed_bootstrap(
        client,
        &ResourceId::local(MIXED_TERMINAL),
        (2, 2),
        (80, 24),
        b"",
    );
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    feed_agent_bootstrap(
        client,
        agent_bootstrap_frames(
            7,
            AgentEventsJsonlResumeV1 { after_seq: 3 },
            4,
            record(4, "tool_end", "{}"),
        ),
    );
    let mut log = Vec::new();
    assert_eq!(
        drain_agent_log(client, &mut log),
        [(AGENT_RECORDS_RETAINED, 6, 3), (AGENT_RECORDS_LIVE, 7, 4)]
    );
    assert_eq!(
        log.iter()
            .map(|line| line["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    // SAFETY: this fixture owns the client through final free.
    unsafe { phux_client_free(client) };
}
