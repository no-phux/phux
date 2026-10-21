//! Agent sessions: catalogued beside terminals, and their record streams.

use super::*;

#[test]
fn agent_sessions_are_catalogued_but_never_gate_the_barrier() {
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
    let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
    let client = attached_mixed_client();
    let inner = unsafe { &(*client).inner };
    assert!(
        inner.attached,
        "the terminal alone completes an attach the agent never bootstrapped"
    );
    assert!(inner.active_attach_contains(&terminal));
    assert!(!inner.active_attach_contains(&agent));
    assert_eq!(
        inner.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    assert!(inner.is_agent_stream(&agent));

    assert_eq!(unsafe { phux_client_resource_count(client) }, 2);
    let mut resource = PhuxResourceInfo::default();
    assert_eq!(
        unsafe { phux_client_resource_get(client, 0, &raw mut resource) },
        PhuxClientResult::Ok
    );
    assert_eq!(resource.kind, RESOURCE_KIND_TERMINAL);
    assert!(resource.parent.is_null());
    assert_eq!(
        resource.provider.len + resource.native_id.len + resource.state.len,
        0
    );
    assert_eq!(
        unsafe { phux_client_resource_get(client, 1, &raw mut resource) },
        PhuxClientResult::Ok
    );
    assert_eq!(resource.kind, RESOURCE_KIND_AGENT_SESSION);
    assert_eq!(
        (resource.terminal_id.kind, resource.terminal_id.id),
        (0, MIXED_AGENT)
    );
    let parent = unsafe { resource.parent.as_ref() }.expect("parent is present");
    assert_eq!((parent.kind, parent.id), (0, MIXED_TERMINAL));
    assert_eq!(span_bytes(resource.provider), b"claude");
    assert_eq!(span_bytes(resource.native_id), b"s-1");
    assert_eq!(span_bytes(resource.state), b"working");

    // A terminal profile on the agent stream, and the agent profile on a
    // terminal, are both protocol errors before the kernel sees them.
    for (terminal_id, profile) in [
        (
            agent,
            phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        ),
        (
            terminal,
            phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
        ),
    ] {
        assert_eq!(
            feed(
                client,
                &FrameKind::BootstrapBegin {
                    terminal_id,
                    stream_id: phux_protocol::StreamId::new(2).expect("stream"),
                    bootstrap_id: phux_protocol::BootstrapId::new(2).expect("bootstrap"),
                    profile,
                    cols: 0,
                    rows: 0,
                    base_seq: 0,
                },
            ),
            PhuxClientResult::ProtocolError
        );
    }
    unsafe { phux_client_free(client) };
}

#[test]
fn agent_stream_frames_surface_as_records_effects() {
    let client = attached_mixed_client();
    open_agent_stream(client);
    assert_eq!(unsafe { phux_client_effect_count(client) }, 2);
    let effect = effect_at(client, 0);
    assert_eq!(
        (effect.kind, effect.detail, effect.seq),
        (EFFECT_AGENT_RECORDS, AGENT_RECORDS_RETAINED, 2)
    );
    assert_eq!((effect.stream_id, effect.bootstrap_id), (4, 5));
    assert_eq!(
        (effect.terminal_id.kind, effect.terminal_id.id),
        (0, MIXED_AGENT)
    );
    let lines: Vec<serde_json::Value> = effect_bytes(&effect)
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("record line is JSON"))
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["type"], "session_start");
    assert_eq!(lines[0]["data"]["provider"], "claude");
    assert_eq!(lines[1]["seq"], 2);
    assert_eq!(lines[1]["ts_ms"], 20);
    let effect = effect_at(client, 1);
    assert_eq!(
        (effect.kind, effect.detail, effect.seq),
        (EFFECT_AGENT_RECORDS, AGENT_RECORDS_LIVE, 3)
    );
    let live: serde_json::Value =
        serde_json::from_slice(effect_bytes(&effect).trim_ascii()).expect("live record");
    assert_eq!(live["type"], "ask");
    unsafe { phux_client_free(client) };
}

#[test]
fn agent_stream_faults_retire_the_generation_and_closes_retire_the_resource() {
    let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
    let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP).expect("bootstrap");
    let client = attached_mixed_client();
    open_agent_stream(client);
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );

    // History is a terminal facet: the kernel refuses it for an agent stream.
    assert_ne!(
        feed(
            client,
            &FrameKind::HistoryRejected {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                cursor: bytes::Bytes::from_static(b"c"),
                reason: phux_protocol::wire::frame::HistoryRejectionReason::Busy,
                required_bytes: 1,
                required_rows: 1,
            },
        ),
        PhuxClientResult::Ok
    );

    // A malformed payload retires the generation with a RESYNC_REQUIRED
    // status instead of handing the host a log with a hole in it.
    assert_ne!(
        feed(
            client,
            &FrameKind::ResourceOutput {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                seq: 4,
                bytes: bytes::Bytes::from_static(b"{not json\n"),
            },
        ),
        PhuxClientResult::Ok
    );
    let count = unsafe { phux_client_effect_count(client) };
    assert!(count >= 1);
    let effect = effect_at(client, count - 1);
    assert_eq!((effect.kind, effect.detail), (2, 3));
    assert_eq!(
        (effect.terminal_id.kind, effect.terminal_id.id),
        (0, MIXED_AGENT)
    );
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );

    // A close retires the resource with a CLOSED effect.
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: agent.clone(),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
                signal: None,
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_resource_count(client) }, 1);
    assert_eq!(unsafe { phux_client_effect_count(client) }, 1);
    let effect = effect_at(client, 0);
    assert_eq!(
        (effect.kind, effect.detail),
        (EFFECT_AGENT_RECORDS, AGENT_RECORDS_CLOSED)
    );
    assert_eq!((effect.stream_id, effect.bootstrap_id), (4, 5));
    assert!(!unsafe { (*client).inner.is_agent_stream(&agent) });
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceOutput {
                terminal_id: agent,
                stream_id,
                bootstrap_id,
                seq: 5,
                bytes: bytes::Bytes::from(record(5, "stop", "{}")),
            },
        ),
        PhuxClientResult::ProtocolError,
        "a closed agent resource is no longer a participant of any kind"
    );
    unsafe { phux_client_free(client) };
}
