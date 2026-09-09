//! Wire fixtures drive the public FFI feed, catalog, and outgoing-frame APIs.
use super::*;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;

fn snapshot(with_agent: bool) -> SessionSnapshot {
    let mut snapshot = mixed_kind_snapshot(
        &ResourceId::local(MIXED_TERMINAL),
        &ResourceId::local(MIXED_AGENT),
    );
    if !with_agent {
        snapshot.resources.truncate(1);
    }
    snapshot
}

fn outgoing(client: *mut PhuxClient) -> Vec<FrameKind> {
    // SAFETY: fixture owns this live client and copies borrowed frames before clearing.
    unsafe {
        let frames = (0..phux_client_outgoing_count(client))
            .map(|index| {
                let mut bytes = PhuxBytes::default();
                assert_eq!(
                    phux_client_outgoing_get(client, index, &raw mut bytes),
                    PhuxClientResult::Ok
                );
                FrameKind::decode(span_bytes(bytes)).unwrap().0
            })
            .collect();
        assert_eq!(phux_client_outgoing_clear(client), PhuxClientResult::Ok);
        frames
    }
}

fn answer_read(client: *mut PhuxClient, snapshot: SessionSnapshot) {
    let requests = outgoing(client);
    let state_id = requests
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command:
                    Command::GetState {
                        scope: StateScope::Server,
                    },
            } => Some(*request_id),
            _ => None,
        })
        .expect("FFI emitted GET_STATE");
    let metadata_id = requests
        .iter()
        .find_map(|frame| match frame {
            FrameKind::GetMetadata { request_id, .. } => Some(*request_id),
            _ => None,
        })
        .expect("FFI emitted metadata read");
    for frame in [
        FrameKind::MetadataValue {
            request_id: metadata_id,
            value: None,
        },
        FrameKind::CommandResult {
            request_id: state_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        },
    ] {
        assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
    }
}

fn refresh(client: *mut PhuxClient, request: u32, snapshot: SessionSnapshot) {
    // SAFETY: fixture owns a live exclusive client.
    assert_eq!(
        unsafe { phux_client_workspace_refresh(client, request) },
        PhuxClientResult::Ok
    );
    answer_read(client, snapshot);
}

fn subscription(client: *mut PhuxClient) -> u32 {
    let frames = outgoing(client);
    assert_eq!(
        frames.len(),
        1,
        "discovery must emit one subscription, not reconnect"
    );
    match &frames[0] {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id },
        } => {
            assert_eq!(terminal_id, &ResourceId::local(MIXED_AGENT));
            *request_id
        }
        other => panic!("expected resource subscription, got {other:?}"),
    }
}

#[test]
fn agent_created_after_attach_is_discovered_subscribed_streamed_and_removed() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    // SAFETY: this fixture owns the client through its final free.
    unsafe {
        assert_eq!(phux_client_resource_count(client), 1);
        let terminal = ResourceId::local(MIXED_TERMINAL);
        let generation = (*client)
            .inner
            .session
            .published(&terminal)
            .unwrap()
            .key()
            .clone();
        refresh(client, 1, snapshot(true));
        assert_eq!(
            phux_client_resource_count(client),
            2,
            "GET_STATE must update the host resource roster"
        );
        let request_id = subscription(client);
        // The reference server sends bootstrap before the command acknowledgement.
        open_agent_stream(client);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok
                }
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_effect_count(client), 2);
        assert_eq!(effect_at(client, 0).detail, AGENT_RECORDS_RETAINED);
        assert_eq!(effect_at(client, 1).detail, AGENT_RECORDS_LIVE);
        assert_eq!(effect_at(client, 1).seq, 3);
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);

        refresh(client, 2, snapshot(true));
        assert!(
            outgoing(client).is_empty(),
            "unchanged inventory must preserve the subscription"
        );
        assert_eq!(
            phux_client_effect_count(client),
            0,
            "refresh must not replay records"
        );
        refresh(client, 3, snapshot(false));
        assert_eq!(phux_client_resource_count(client), 1);
        assert_eq!(phux_client_effect_count(client), 1);
        let closed = effect_at(client, 0);
        assert_eq!(
            (closed.kind, closed.detail),
            (EFFECT_AGENT_RECORDS, AGENT_RECORDS_CLOSED)
        );
        assert_eq!(
            (closed.stream_id, closed.bootstrap_id),
            (AGENT_STREAM, AGENT_BOOTSTRAP)
        );
        assert!(
            !(*client)
                .inner
                .is_agent_stream(&ResourceId::local(MIXED_AGENT))
        );
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceClosed {
                    terminal_id: ResourceId::local(MIXED_AGENT),
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
                }
            ),
            PhuxClientResult::Ok,
            "a close racing an authoritative removal is idempotent"
        );
        assert_eq!(phux_client_effect_count(client), 0);
        assert_eq!(
            (*client).inner.session.published(&terminal).unwrap().key(),
            &generation
        );
        phux_client_free(client);
    }
}

#[test]
fn initial_agent_inventory_also_needs_a_subscription() {
    let client = attached_mixed_client();
    answer_read(client, snapshot(true));
    subscription(client);
    // SAFETY: fixture owns this client.
    unsafe { phux_client_free(client) };
}

#[test]
fn refresh_preserves_an_agent_replacement_generation_in_flight() {
    let client = attached_mixed_client();
    answer_read(client, snapshot(true));
    let request_id = subscription(client);
    open_agent_stream(client);
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok
            }
        ),
        PhuxClientResult::Ok
    );
    let agent = ResourceId::local(MIXED_AGENT);
    let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).unwrap();
    let bootstrap_id = phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP + 1).unwrap();
    // SAFETY: fixture owns the client through its final free.
    unsafe {
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
                cols: 0,
                rows: 0,
                base_seq: 4,
            },
            FrameKind::BootstrapChunk {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from(record(4, "stop", "{}")),
            },
        ] {
            assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
        }
        refresh(client, 1, snapshot(true));
        assert!(outgoing(client).is_empty());
        assert_eq!(phux_client_effect_count(client), 0);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapReady {
                    terminal_id: agent,
                    stream_id,
                    bootstrap_id,
                    history_cursor: None
                }
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_effect_count(client), 1);
        let effect = effect_at(client, 0);
        assert_eq!(
            (effect.kind, effect.detail, effect.seq),
            (EFFECT_AGENT_RECORDS, AGENT_RECORDS_RETAINED, 4)
        );
        assert_eq!(
            (effect.stream_id, effect.bootstrap_id),
            (AGENT_STREAM, AGENT_BOOTSTRAP + 1)
        );
        let record: serde_json::Value =
            serde_json::from_slice(effect_bytes(&effect).trim_ascii()).unwrap();
        assert_eq!(record["type"], "stop");
        phux_client_free(client);
    }
}

#[test]
fn a_refused_agent_subscription_retries_without_losing_authoritative_membership() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    refresh(client, 1, snapshot(true));
    let request_id = subscription(client);
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: phux_protocol::wire::frame::ErrorCode::ResourceExhausted,
                    message: "busy cutting bootstrap".into(),
                },
            }
        ),
        PhuxClientResult::Ok
    );
    // SAFETY: fixture owns this client.
    unsafe {
        assert_eq!(
            phux_client_resource_count(client),
            2,
            "a command refusal cannot retract registry membership"
        );
        assert_eq!(
            phux_client_effect_count(client),
            0,
            "refusal is not resource closure"
        );
        refresh(client, 2, snapshot(true));
        assert!(subscription(client) > request_id);
        open_agent_stream(client);
        assert_eq!(
            effect_at(client, phux_client_effect_count(client) - 1).detail,
            AGENT_RECORDS_LIVE
        );
        phux_client_free(client);
    }
}

#[test]
fn unknown_resource_membership_is_refreshed_without_subscribing_or_closing_a_replica() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    let mut registry = snapshot(false);
    registry.resources.push(
        phux_protocol::wire::info::ResourceInfo::new(
            ResourceId::local(90),
            phux_protocol::WindowId::new(0),
            0,
            0,
        )
        .with_kind(ResourceKind::Unknown { tag: 90 }),
    );
    refresh(client, 1, registry);
    // SAFETY: fixture owns this client and copies borrowed fields before mutation.
    unsafe {
        assert_eq!(phux_client_resource_count(client), 2);
        let mut resource = PhuxResourceInfo::default();
        assert_eq!(
            phux_client_resource_get(client, 1, &raw mut resource),
            PhuxClientResult::Ok
        );
        assert_eq!(resource.kind, 90);
        assert!(outgoing(client).is_empty());
        refresh(client, 2, snapshot(false));
        assert_eq!(phux_client_resource_count(client), 1);
        assert_eq!(phux_client_effect_count(client), 0);
        phux_client_free(client);
    }
}

#[test]
fn agent_discovery_does_not_spend_dynamic_terminal_admission() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    // Fill the real terminal admission table through the ABI, acknowledging each
    // request without allocating a grid. The agent must still be subscribable.
    // SAFETY: fixture owns the client and all stack options through each call.
    unsafe {
        for index in 0..operations::MAX_DYNAMIC_TERMINALS {
            let request_id = u32::try_from(index + 1).unwrap();
            let options = PhuxAttachResourceOptions {
                request_id,
                terminal_id: terminal_id_out(&ResourceId::local(1000 + request_id)),
                ..PhuxAttachResourceOptions::default()
            };
            assert_eq!(
                phux_client_queue_attach_resource(client, &raw const options),
                PhuxClientResult::Ok
            );
            outgoing(client);
            assert_eq!(
                feed_kind(
                    client,
                    &FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::Ok
                    }
                ),
                PhuxClientResult::Ok
            );
            assert_eq!(phux_client_operation_clear(client), PhuxClientResult::Ok);
        }
        refresh(client, 1000, snapshot(true));
        subscription(client);
        open_agent_stream(client);
        assert_eq!(effect_at(client, 1).detail, AGENT_RECORDS_LIVE);
        phux_client_free(client);
    }
}
