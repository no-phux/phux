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
    answer_requests(client, &requests, snapshot, None);
}

fn answer_requests(
    client: *mut PhuxClient,
    requests: &[FrameKind],
    snapshot: SessionSnapshot,
    metadata: Option<Vec<u8>>,
) {
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
            value: metadata,
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
    subscription_for(client, &ResourceId::local(MIXED_AGENT))
}

fn subscription_for(client: *mut PhuxClient, id: &ResourceId) -> u32 {
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
            assert_eq!(terminal_id, id);
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
        refresh(client, 4, snapshot(true));
        assert!(
            outgoing(client).is_empty(),
            "explicit closure must prevent same-ID resubscription"
        );
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
fn explicit_agent_close_cannot_be_undone_by_an_outstanding_subscription_refusal() {
    let client = attached_mixed_client();
    answer_read(client, snapshot(true));
    let request_id = subscription(client);
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(MIXED_AGENT),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
            }
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(
        feed_kind(client, &subscription_refusal(request_id, false)),
        PhuxClientResult::Ok
    );
    refresh(client, 1, snapshot(true));
    assert!(
        outgoing(client).is_empty(),
        "late refusal must not release the explicit-close tombstone"
    );
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

fn workspace_info(client: *mut PhuxClient) -> PhuxWorkspaceInfo {
    let mut info = PhuxWorkspaceInfo::default();
    // SAFETY: fixture owns the client and the disjoint output record.
    assert_eq!(
        unsafe { phux_client_workspace_info(client, &raw mut info) },
        PhuxClientResult::Ok
    );
    info
}

fn subscription_refusal(request_id: u32, standalone_error: bool) -> FrameKind {
    let code = phux_protocol::wire::frame::ErrorCode::ResourceExhausted;
    let message = "subscription temporarily unavailable".into();
    if standalone_error {
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        }
    } else {
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error { code, message },
        }
    }
}

#[test]
fn older_subscription_refusal_does_not_cancel_a_newer_workspace_mutation() {
    assert_subscription_refusal_preserves_mutation(false);
}

#[test]
fn older_subscription_error_frame_does_not_cancel_a_newer_workspace_mutation() {
    assert_subscription_refusal_preserves_mutation(true);
}

fn assert_subscription_refusal_preserves_mutation(standalone_error: bool) {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    refresh(client, 1, snapshot(true));
    let subscription_id = subscription(client);
    let before = workspace_info(client);
    // SAFETY: fixture owns this live client and all borrowed options/output spans.
    unsafe {
        let mut window = PhuxWorkspaceWindow::default();
        assert_eq!(
            phux_client_workspace_window_get(client, 0, &raw mut window),
            PhuxClientResult::Ok
        );
        let mutation = PhuxWorkspaceMutation {
            request_id: 2,
            expected_revision: before.revision,
            session_id: before.session_id,
            kind: 6,
            window_id: window.window_id,
            name: bytes_out(b"confirmed rename"),
            ..PhuxWorkspaceMutation::default()
        };
        assert_eq!(
            phux_client_workspace_mutate(client, &raw const mutation),
            PhuxClientResult::Ok
        );
        let requests = outgoing(client);
        assert!(
            requests
                .iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { .. })),
            "the new mutation reached the transport before the old refusal"
        );
        assert_eq!(
            feed_kind(
                client,
                &subscription_refusal(subscription_id, standalone_error)
            ),
            PhuxClientResult::Ok
        );
        let pending = workspace_info(client);
        assert_eq!(
            (pending.request_id, pending.status),
            (2, 1),
            "an older subscription failure must not fail workspace request 2"
        );
        assert_eq!(pending.revision, before.revision);
        // Independent server-confirmation fixture, not a replay of the outgoing SET bytes.
        let confirmed = phux_client_core::layout::Workspace {
            windows: vec![phux_client_core::layout::WindowState {
                id: window.window_id,
                name: "confirmed rename".into(),
                state: phux_client_core::layout::LayoutState::single(ResourceId::local(
                    MIXED_TERMINAL,
                )),
            }],
            active: 0,
        };
        answer_requests(
            client,
            &requests,
            snapshot(true),
            Some(confirmed.encode_topology_cbor().unwrap()),
        );
        let completed = workspace_info(client);
        assert_eq!((completed.request_id, completed.status), (2, 2));
        assert!(completed.revision > before.revision);
        assert_eq!(
            phux_client_workspace_window_get(client, 0, &raw mut window),
            PhuxClientResult::Ok
        );
        assert_eq!(span_bytes(window.name), b"confirmed rename");
        phux_client_free(client);
    }
}

fn satellite_snapshot() -> SessionSnapshot {
    let parent = ResourceId::satellite("review-satellite", MIXED_TERMINAL);
    let agent = ResourceId::satellite("review-satellite", MIXED_AGENT);
    let mut registry = snapshot(false);
    registry
        .resources
        .extend(mixed_kind_snapshot(&parent, &agent).resources);
    registry
}

fn bootstrap_agent(
    client: *mut PhuxClient,
    agent: &ResourceId,
    generation: u64,
    seq: u64,
    kind: &str,
) {
    let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).unwrap();
    let bootstrap_id = phux_protocol::BootstrapId::new(generation).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::from(record(seq, kind, "{}")),
        },
        FrameKind::BootstrapReady {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
    }
}

#[test]
fn satellite_inventory_withdrawal_allows_same_agent_to_return_with_a_fresh_generation() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    let agent = ResourceId::satellite("review-satellite", MIXED_AGENT);
    refresh(client, 1, satellite_snapshot());
    let request_id = subscription_for(client, &agent);
    bootstrap_agent(client, &agent, AGENT_BOOTSTRAP, 1, "ask");
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
    // SAFETY: fixture owns this client through its final free.
    unsafe {
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
        // Federated GET_STATE succeeds with an empty satellite contribution on
        // relay failure. This is membership withdrawal, not ResourceClosed.
        refresh(client, 2, snapshot(false));
        assert_eq!(phux_client_resource_count(client), 1);
        assert_eq!(phux_client_effect_count(client), 1);
        assert_eq!(effect_at(client, 0).detail, AGENT_RECORDS_CLOSED);
        refresh(client, 3, snapshot(false));
        assert_eq!(phux_client_effect_count(client), 1, "withdraw only once");
        refresh(client, 4, satellite_snapshot());
        subscription_for(client, &agent);
        bootstrap_agent(client, &agent, AGENT_BOOTSTRAP + 1, 2, "stop");
        assert_eq!(phux_client_resource_count(client), 3);
        assert_eq!(phux_client_effect_count(client), 2);
        assert_eq!(effect_at(client, 0).detail, AGENT_RECORDS_CLOSED);
        let replacement = effect_at(client, 1);
        assert_eq!(replacement.seq, 2);
        assert_eq!(
            (replacement.detail, replacement.bootstrap_id),
            (AGENT_RECORDS_RETAINED, AGENT_BOOTSTRAP + 1)
        );
        let record: serde_json::Value =
            serde_json::from_slice(effect_bytes(&replacement).trim_ascii()).unwrap();
        assert_eq!(record["type"], "stop");
        phux_client_free(client);
    }
}

#[test]
fn discovered_agent_delivers_multi_record_live_batch_at_its_final_sequence() {
    let client = attached_resource_client(snapshot(false));
    answer_read(client, snapshot(false));
    refresh(client, 1, snapshot(true));
    subscription(client);
    let agent = ResourceId::local(MIXED_AGENT);
    bootstrap_agent(client, &agent, AGENT_BOOTSTRAP, 1, "prompt");
    // SAFETY: fixture owns this client and effects through final free.
    unsafe {
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
        let payload = record(2, "ask", "{}") + &record(3, "stop", "{}");
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceOutput {
                    terminal_id: agent,
                    stream_id: phux_protocol::StreamId::new(AGENT_STREAM).unwrap(),
                    bootstrap_id: phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP).unwrap(),
                    seq: 3,
                    bytes: bytes::Bytes::from(payload),
                }
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_effect_count(client), 1);
        let effect = effect_at(client, 0);
        assert_eq!((effect.detail, effect.seq), (AGENT_RECORDS_LIVE, 3));
        let records: Vec<serde_json::Value> = effect_bytes(&effect)
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["seq"], 2);
        assert_eq!(records[1]["seq"], 3);
        assert_eq!(records[1]["type"], "stop");
        phux_client_free(client);
    }
}

#[test]
fn stale_inventory_cannot_resurrect_a_closed_agent_or_block_later_discovery() {
    assert_closed_agent_precedes_stale_inventory(false);
}

#[test]
fn stale_inventory_cannot_resurrect_an_agent_closed_after_withdrawal() {
    assert_closed_agent_precedes_stale_inventory(true);
}

fn assert_closed_agent_precedes_stale_inventory(withdraw_first: bool) {
    let client = attached_mixed_client();
    answer_read(client, snapshot(true));
    subscription(client);
    if withdraw_first {
        refresh(client, 1, snapshot(false));
    }
    let fresh_id = ResourceId::local(MIXED_AGENT + 1);
    let mut stale = snapshot(true);
    let mut fresh = stale.resources[1].clone();
    fresh.id = fresh_id.clone();
    stale.resources.push(fresh);
    // SAFETY: fixture owns this client and output records through final free.
    unsafe {
        assert_eq!(
            phux_client_workspace_refresh(client, 2),
            PhuxClientResult::Ok
        );
        let requests = outgoing(client);
        // Federated GET_STATE captures local inventory before awaiting satellites;
        // a local close can reach the subscriber before that stale result.
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceClosed {
                    terminal_id: ResourceId::local(MIXED_AGENT),
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
                }
            ),
            PhuxClientResult::Ok
        );
        answer_requests(client, &requests, stale, None);
        let info = workspace_info(client);
        assert_eq!(
            (info.request_id, info.status),
            (2, 2),
            "a stale closed entry must not fail refresh or later discovery"
        );
        assert_eq!(phux_client_resource_count(client), 2);
        let mut resource = PhuxResourceInfo::default();
        assert_eq!(
            phux_client_resource_get(client, 1, &raw mut resource),
            PhuxClientResult::Ok
        );
        assert_eq!(
            resource.terminal_id.id,
            MIXED_AGENT + 1,
            "closed agent stays absent"
        );
        subscription_for(client, &fresh_id);
        bootstrap_agent(client, &fresh_id, AGENT_BOOTSTRAP, 1, "ask");
        phux_client_free(client);
    }
}
