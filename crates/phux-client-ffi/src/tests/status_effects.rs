//! PHA-406/PHA-284: the bridge subscribes to the connection-wide event
//! stream after attach and folds cwd/command/exit events into typed status
//! effects (`docs/consumers/cockpit.md` "Status effects").
use super::*;
use phux_protocol::wire::frame::{AgentEvent, CloseReason};
use std::ptr;

/// After every attach completes, the bridge asks the server for the events
/// its status effects depend on — without this, `cwd_changed` /
/// `command_started` / `command_finished` / `terminal_control` never arrive.
#[test]
fn ffi_client_subscribes_to_events_after_attach() {
    let client = attached_mixed_client();
    let sent: Vec<FrameKind> = unsafe {
        (0..phux_client_outgoing_count(client))
            .map(|index| {
                let mut bytes = PhuxBytes::default();
                assert_eq!(
                    phux_client_outgoing_get(client, index, &raw mut bytes),
                    PhuxClientResult::Ok
                );
                FrameKind::decode(span_bytes(bytes)).unwrap().0
            })
            .collect()
    };
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None
            }
        )),
        "ATTACH_READY must be followed by SUBSCRIBE_EVENTS{{terminal: None}}: {sent:?}"
    );
    unsafe { phux_client_free(client) };
}

/// A host that arms a journal cursor before ATTACH_READY gets `after_seq` on
/// the automatic subscribe, so a reconnect can resume instead of going live-only.
#[test]
fn ffi_client_subscribes_from_after_seq_when_armed_before_attach() {
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
    let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
    let snapshot = mixed_kind_snapshot(&terminal, &agent);
    let stream_id = phux_protocol::StreamId::new(1).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
    let client = boxed_client();
    let after_seq = 41u64;
    unsafe {
        (*client).inner.protocol_ready = true;
        (*client).inner.event_journal = true;
        (*client).inner.attach_queued = true;
        (*client).inner.expected_attach_id = Some(7);
        (*client).inner.selected_profile = Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        assert_eq!(
            phux_client_subscribe_events(client, ptr::null(), &raw const after_seq),
            PhuxClientResult::Ok
        );
    }
    for frame in [
        FrameKind::Attached {
            attach_id: 7,
            snapshot,
            initial_client_id: phux_protocol::ClientId::new(9),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id: 7 },
    ] {
        assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
    }
    let sent: Vec<FrameKind> = unsafe {
        (0..phux_client_outgoing_count(client))
            .map(|index| {
                let mut bytes = PhuxBytes::default();
                assert_eq!(
                    phux_client_outgoing_get(client, index, &raw mut bytes),
                    PhuxClientResult::Ok
                );
                FrameKind::decode(span_bytes(bytes)).unwrap().0
            })
            .collect()
    };
    assert!(
        sent.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(41)
            }
        )),
        "armed after_seq must reach SUBSCRIBE_EVENTS: {sent:?}"
    );
    unsafe { phux_client_free(client) };
}

/// An already-attached client can re-subscribe with a cursor without re-attaching.
#[test]
fn ffi_client_subscribe_events_queues_after_seq_while_attached() {
    let client = attached_mixed_client();
    unsafe {
        (*client).inner.event_journal = true;
        (*client).inner.outgoing.clear();
        let after_seq = u64::MAX;
        assert_eq!(
            phux_client_subscribe_events(client, ptr::null(), &raw const after_seq),
            PhuxClientResult::Ok
        );
        let mut bytes = PhuxBytes::default();
        assert_eq!(
            phux_client_outgoing_get(client, 0, &raw mut bytes),
            PhuxClientResult::Ok
        );
        let (frame, remaining) = FrameKind::decode(span_bytes(bytes)).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(
            frame,
            FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: Some(u64::MAX),
            }
        );
        phux_client_free(client);
    }
}

/// An effect's fields copied out of the client's transient effect storage,
/// so they outlive the `phux_client_effect_clear` that a fixture needs
/// between two scoped events — unlike `PhuxClientEffect`, whose `bytes`
/// pointer is only valid until the next mutable client call.
struct CapturedStatus {
    kind: u32,
    detail: u32,
    stream_id: u64,
    bootstrap_id: u64,
    bytes: Vec<u8>,
}

/// Feeds one `AgentEvent` scoped to `terminal`, asserts the feed produced
/// exactly `expected_count` effects, and captures the effect at index 0
/// before clearing the client's effect queue for the next call.
fn feed_scoped_event(
    client: *mut PhuxClient,
    terminal: &phux_protocol::ResourceId,
    event: AgentEvent,
    expected_count: usize,
) -> CapturedStatus {
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::Event {
                terminal: Some(terminal.clone()),
                event,
                stamp: None,
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_effect_count(client) }, expected_count);
    let effect = effect_at(client, 0);
    let captured = CapturedStatus {
        kind: effect.kind,
        detail: effect.detail,
        stream_id: effect.stream_id,
        bootstrap_id: effect.bootstrap_id,
        bytes: span_bytes(effect.bytes).to_vec(),
    };
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );
    captured
}

#[test]
fn cwd_changed_becomes_a_cwd_status_effect() {
    let client = attached_mixed_client();
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
    let effect = feed_scoped_event(
        client,
        &terminal,
        AgentEvent::CwdChanged {
            cwd: "/srv/work".to_owned(),
        },
        1,
    );
    assert_eq!((effect.kind, effect.detail), (2, 8));
    assert_eq!(effect.bytes, b"/srv/work");
    unsafe { phux_client_free(client) };
}

#[test]
fn command_started_and_finished_become_status_effects() {
    let client = attached_mixed_client();
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);

    let started = feed_scoped_event(client, &terminal, AgentEvent::CommandStarted, 1);
    assert_eq!((started.kind, started.detail), (2, 9));

    let finished = feed_scoped_event(
        client,
        &terminal,
        AgentEvent::CommandFinished { exit_code: Some(3) },
        1,
    );
    assert_eq!((finished.kind, finished.detail), (2, 10));
    assert_eq!((finished.stream_id, finished.bootstrap_id), (1, 3));

    unsafe { phux_client_free(client) };
}

/// A server-scoped event and an event kind this lane does not surface as
/// status are both silent no-ops: no effect, no error.
#[test]
fn unrecognised_and_untargeted_events_produce_no_effect() {
    let client = attached_mixed_client();
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);

    assert_eq!(
        feed_kind(
            client,
            &FrameKind::Event {
                terminal: None,
                event: AgentEvent::CwdChanged {
                    cwd: "/should-be-ignored".to_owned(),
                },
                stamp: None,
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::Bell,
                stamp: None,
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_effect_count(client) }, 0);
    unsafe { phux_client_free(client) };
}

/// A plain `RESOURCE_CLOSED` becomes an `EXITED` status carrying the
/// frame's exit code and reason, alongside the damage it already produced
/// before this lane.
#[test]
fn resource_closed_becomes_an_exited_status_effect() {
    let client = attached_mixed_client();
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);

    assert_eq!(
        feed_kind(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: terminal,
                exit_status: Some(0),
                reason: CloseReason::Exited,
                signal: None,
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_effect_count(client) }, 2);
    let effect = effect_at(client, 0);
    assert_eq!((effect.kind, effect.detail), (2, 11));
    assert_eq!(effect.status_code, u32::from(CloseReason::Exited.as_wire()));
    assert_eq!((effect.stream_id, effect.bootstrap_id), (1, 0));
    assert_eq!(effect.first_row, 0);

    unsafe { phux_client_free(client) };
}

/// A `RESOURCE_CLOSED` naming a terminating signal surfaces it on EXITED's
/// `first_row` (`docs/spec/L1.md` `RESOURCE_CLOSED` field 4, ADR-0124).
#[test]
fn resource_closed_with_a_signal_surfaces_it_on_the_exited_status_effect() {
    let client = attached_mixed_client();
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);

    assert_eq!(
        feed_kind(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: terminal,
                exit_status: None,
                reason: CloseReason::Killed,
                signal: Some(9),
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_effect_count(client) }, 2);
    let effect = effect_at(client, 0);
    assert_eq!((effect.kind, effect.detail), (2, 11));
    assert_eq!(effect.status_code, u32::from(CloseReason::Killed.as_wire()));
    assert_eq!(effect.first_row, 9, "signal 9 must reach EXITED.first_row");

    unsafe { phux_client_free(client) };
}
