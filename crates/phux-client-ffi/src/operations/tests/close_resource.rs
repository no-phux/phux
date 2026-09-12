use super::*;

fn satellite_attachment(bound: bool) -> (Harness, ResourceId) {
    let mut h = Harness::attached();
    h.0.inner.conditional_kill = true;
    let options = PhuxSpawnOptions {
        request_id: 1,
        satellite: bytes_out(b"sat"),
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: harness owns client; options and its static host span are readable.
    unsafe {
        let queued = if bound {
            phux_client_queue_spawn_bound(h.ptr(), &raw const options)
        } else {
            phux_client_queue_spawn(h.ptr(), &raw const options)
        };
        assert_eq!(queued, PhuxClientResult::Ok);
    }
    let id = ResourceId::satellite(SatelliteHost::new("sat"), 9);
    let spawned = if bound {
        SpawnResult::OkBound {
            id: id.clone(),
            instance: ServerInstance::new([7; 16]),
        }
    } else {
        SpawnResult::Ok(id.clone())
    };
    assert_eq!(
        h.feed(FrameKind::ResourceSpawned {
            request_id: 1,
            result: spawned
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(h.attach(2, &id), PhuxClientResult::Ok);
    h.bootstrap(id.clone());
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    // SAFETY: harness owns client; clear prior receipts before testing refusal.
    unsafe {
        assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
    }
    h.0.inner.outgoing.clear();
    (h, id)
}

#[test]
fn satellite_close_without_instance_is_refused_despite_current_hub_attachment() {
    let (mut h, id) = satellite_attachment(false);
    let raw = terminal_id_out(&id);
    assert!(h.0.inner.operations.admitted(&id));
    assert!(h.0.inner.session.published(&id).is_some());
    // SAFETY: live owned client and readable satellite ID.
    unsafe {
        assert_eq!(
            phux_client_queue_close_resource(h.ptr(), 3, &raw const raw),
            PhuxClientResult::InvalidState
        );
    }
    assert_eq!(
        h.0.inner.last_error,
        b"satellite close requires an instance-bound resource; no safe incarnation fence"
    );
    assert!(h.0.inner.outgoing.is_empty());
    assert!(h.0.inner.operations.pending.is_empty());
    assert!(h.0.inner.operations.completed.is_empty());
    assert!(h.0.inner.session.published(&id).is_some());
    assert_eq!(
        close(&mut h, 3, 1),
        PhuxClientResult::Ok,
        "refusal did not consume request ID"
    );
}

#[test]
fn satellite_close_preserves_bound_instance_and_authoritative_refusal() {
    let (mut h, id) = satellite_attachment(true);
    let raw = terminal_id_out(&id);
    // SAFETY: live owned client and readable satellite ID.
    unsafe {
        assert_eq!(
            phux_client_queue_close_resource(h.ptr(), 3, &raw const raw),
            PhuxClientResult::Ok
        );
    }
    assert_eq!(
        FrameKind::decode(&h.0.inner.outgoing[0]).unwrap().0,
        FrameKind::Command {
            request_id: 3,
            command: Command::KillResourceIf {
                terminal_id: id.clone(),
                precondition: KillPrecondition {
                    instance: Some(ServerInstance::new([7; 16])),
                    conditions: phux_protocol::wire::frame::KillConditions::NONE
                }
            }
        }
    );
    h.0.inner.outgoing.clear();
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 3,
            result: CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message: "the instance token no longer names this server's id space".into()
            }
        }),
        PhuxClientResult::Ok
    );
    let out = result(&mut h, 0);
    assert_eq!(
        (out.kind, out.status, out.error_code),
        (5, 2, u32::from(ErrorCode::PreconditionFailed.as_wire()))
    );
    assert!(h.0.inner.session.published(&id).is_some());
}

#[test]
fn satellite_and_mixed_batches_are_refused_without_closing_local_prefix() {
    // Even a bound satellite cannot join the wire's uncorrelated batch relay.
    let (mut h, satellite) = satellite_attachment(true);
    let local = ResourceId::local(1);
    let ids = [terminal_id_out(&local), terminal_id_out(&satellite)];
    for batch in [&ids[..], &ids[1..]] {
        // SAFETY: owned client and readable ID records/host spans.
        unsafe {
            assert_eq!(
                phux_client_queue_close_resources(h.ptr(), 3, batch.as_ptr(), batch.len()),
                PhuxClientResult::InvalidState
            );
        }
        assert_eq!(
            h.0.inner.last_error,
            b"atomic close is unavailable for satellite resources; batch was not queued"
        );
        assert!(h.0.inner.outgoing.is_empty());
        assert!(h.0.inner.operations.pending.is_empty());
        assert!(h.0.inner.operations.completed.is_empty());
        assert!(h.0.inner.session.published(&local).is_some());
        assert!(h.0.inner.session.published(&satellite).is_some());
    }
    assert_eq!(
        close_many(&mut h, 3, &[1]),
        PhuxClientResult::Ok,
        "whole-batch refusal consumed no request ID"
    );
}

fn close_many(h: &mut Harness, request: u32, ids: &[u32]) -> PhuxClientResult {
    let ids: Vec<_> = ids
        .iter()
        .map(|id| terminal_id_out(&ResourceId::local(*id)))
        .collect();
    // SAFETY: harness owns client; ID records live through the call.
    unsafe { phux_client_queue_close_resources(h.ptr(), request, ids.as_ptr(), ids.len()) }
}

#[test]
fn successful_abandonment_cleanup_releases_unsubscribed_binding_capacity() {
    let mut h = Harness::attached();
    h.0.inner.conditional_kill = true;
    let token = [7; 16];
    for id in 1..=257 {
        let options = PhuxSpawnOptions {
            request_id: 2 * id - 1,
            satellite: bytes_out(b"sat"),
            ..PhuxSpawnOptions::default()
        };
        // SAFETY: harness owns client and options, with a static host span.
        unsafe {
            assert_eq!(
                phux_client_queue_spawn_bound(h.ptr(), &raw const options),
                PhuxClientResult::Ok
            );
        }
        let terminal = ResourceId::satellite(SatelliteHost::new("sat"), id);
        assert_eq!(
            h.feed(FrameKind::ResourceSpawned {
                request_id: options.request_id,
                result: SpawnResult::OkBound {
                    id: terminal.clone(),
                    instance: ServerInstance::new(token)
                }
            }),
            PhuxClientResult::Ok
        );
        let raw = terminal_id_out(&terminal);
        // SAFETY: live client and borrowed ID/token outlive the calls.
        unsafe {
            assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
            assert_eq!(
                phux_client_queue_kill_if(h.ptr(), 2 * id, &raw const raw, token.as_ptr()),
                PhuxClientResult::Ok
            );
        }
        assert_eq!(
            h.feed(FrameKind::CommandResult {
                request_id: 2 * id,
                result: CommandResult::Ok
            }),
            PhuxClientResult::Ok
        );
        assert!(h.0.inner.operations.instances.is_empty());
        // SAFETY: harness owns client.
        unsafe {
            assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
        }
        h.0.inner.outgoing.clear();
    }
}

#[test]
fn batch_close_validates_every_owner_before_one_atomic_command() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(1), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(ResourceId::local(9))
        }),
        PhuxClientResult::Ok
    );
    h.bootstrap(ResourceId::local(9));
    h.0.inner.outgoing.clear();
    // SAFETY: harness owns client.
    unsafe {
        assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
    }
    assert_eq!(
        close_many(&mut h, 2, &[]),
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(
        close_many(&mut h, 2, &[1, 1]),
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(
        close_many(&mut h, 2, &[1, 999]),
        PhuxClientResult::InvalidState
    );
    assert!(
        h.0.inner.outgoing.is_empty(),
        "invalid suffix must not kill valid prefix"
    );
    assert_eq!(close_many(&mut h, 2, &[1, 9]), PhuxClientResult::Ok);
    assert_eq!(h.0.inner.outgoing.len(), 1);
    assert_eq!(
        FrameKind::decode(&h.0.inner.outgoing[0]).unwrap().0,
        FrameKind::Command {
            request_id: 2,
            command: Command::KillResources {
                ids: vec![ResourceId::local(1), ResourceId::local(9)]
            }
        }
    );
    assert_eq!(close(&mut h, 3, 9), PhuxClientResult::InvalidState);
    assert_eq!(close_many(&mut h, 3, &[1]), PhuxClientResult::InvalidState);
    h.0.inner.outgoing.clear();
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message: "batch refused".into()
            }
        }),
        PhuxClientResult::Ok
    );
    let out = result(&mut h, 0);
    assert_eq!((out.kind, out.status, out.terminal_id.id), (6, 2, 0));
    assert!(h.0.inner.session.published(&ResourceId::local(1)).is_some());
    assert!(h.0.inner.session.published(&ResourceId::local(9)).is_some());
    assert_eq!(close_many(&mut h, 3, &[1, 9]), PhuxClientResult::Ok);
    // SAFETY: harness owns client. Pending batches share the no-replay fence.
    unsafe {
        assert_eq!(phux_client_disconnect(h.ptr()), PhuxClientResult::Ok);
    }
    assert_eq!((result(&mut h, 1).kind, result(&mut h, 1).status), (6, 3));
    assert!(h.0.inner.outgoing.is_empty());
}

fn close(h: &mut Harness, request: u32, id: u32) -> PhuxClientResult {
    let id = terminal_id_out(&ResourceId::local(id));
    // SAFETY: harness owns client; stack ID outlives the call.
    unsafe { phux_client_queue_close_resource(h.ptr(), request, &raw const id) }
}

fn acknowledge(h: &mut Harness, request_id: u32) {
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }),
        PhuxClientResult::Ok
    );
}

fn resource_closed(h: &mut Harness, id: u32) {
    assert_eq!(
        h.feed(FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(id),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Killed,
        }),
        PhuxClientResult::Ok
    );
}

fn two_terminals() -> Harness {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(1), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(ResourceId::local(9)),
        }),
        PhuxClientResult::Ok
    );
    h.bootstrap(ResourceId::local(9));
    // SAFETY: harness owns the client; only the completed spawn is cleared.
    unsafe {
        assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
    }
    h.0.inner.outgoing.clear();
    h
}

#[test]
fn explicit_close_completion_requires_ack_and_closed_in_either_order() {
    for ack_first in [false, true] {
        let mut h = Harness::attached();
        assert_eq!(close(&mut h, 1, 1), PhuxClientResult::Ok);
        if ack_first {
            acknowledge(&mut h, 1);
        } else {
            resource_closed(&mut h, 1);
        }
        assert!(h.0.inner.operations.completed.is_empty());
        assert_eq!(h.0.inner.operations.pending.len(), 1);
        // Clearing results never discards pending close evidence or interest.
        // SAFETY: harness owns its Client on this thread.
        unsafe {
            assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
        }
        if ack_first {
            resource_closed(&mut h, 1);
        } else {
            acknowledge(&mut h, 1);
        }
        let out = result(&mut h, 0);
        assert_eq!((out.kind, out.status, out.terminal_id.id), (5, 1, 1));
        assert!(h.0.inner.operations.pending.is_empty());
        assert!(h.0.inner.session.published(&ResourceId::local(1)).is_none());
    }
}

#[test]
fn batch_close_completion_waits_for_every_resource_even_after_dynamic_release() {
    for ack_first in [false, true] {
        let mut h = two_terminals();
        assert_eq!(close_many(&mut h, 2, &[1, 9]), PhuxClientResult::Ok);
        if ack_first {
            acknowledge(&mut h, 2);
        }
        resource_closed(&mut h, 9);
        assert!(!h.0.inner.operations.admitted(&ResourceId::local(9)));
        assert!(h.0.inner.operations.completed.is_empty());
        assert!(h.0.inner.session.published(&ResourceId::local(1)).is_some());
        resource_closed(&mut h, 1);
        if !ack_first {
            assert!(h.0.inner.operations.completed.is_empty());
            acknowledge(&mut h, 2);
        }
        let out = result(&mut h, 0);
        assert_eq!((out.kind, out.status, out.terminal_id.id), (6, 1, 0));
        assert!(h.0.inner.operations.pending.is_empty());
    }
}

#[test]
fn close_ack_without_all_closures_disconnects_unknown_and_never_replays() {
    let mut h = two_terminals();
    assert_eq!(close_many(&mut h, 2, &[1, 9]), PhuxClientResult::Ok);
    acknowledge(&mut h, 2);
    resource_closed(&mut h, 9);
    assert!(h.0.inner.operations.completed.is_empty());
    // SAFETY: harness exclusively owns this Client.
    unsafe {
        assert_eq!(phux_client_disconnect(h.ptr()), PhuxClientResult::Ok);
    }
    let out = result(&mut h, 0);
    assert_eq!((out.kind, out.status), (6, 3));
    assert!(h.0.inner.outgoing.is_empty());
    assert!(h.0.inner.operations.pending.is_empty());
    assert_eq!(close(&mut h, 3, 1), PhuxClientResult::InvalidState);
}

#[test]
fn independent_client_closure_cannot_complete_colliding_close_request() {
    let mut original = Harness::attached();
    let mut replacement = Harness::attached();
    assert_eq!(close(&mut original, 1, 1), PhuxClientResult::Ok);
    assert_eq!(close(&mut replacement, 1, 1), PhuxClientResult::Ok);
    acknowledge(&mut original, 1);
    acknowledge(&mut replacement, 1);
    resource_closed(&mut replacement, 1);
    assert_eq!(result(&mut replacement, 0).status, 1);
    assert!(original.0.inner.operations.completed.is_empty());
    assert!(
        original
            .0
            .inner
            .session
            .published(&ResourceId::local(1))
            .is_some()
    );
    resource_closed(&mut original, 1);
    assert_eq!(result(&mut original, 0).status, 1);
}

#[test]
fn close_pending_admission_release_is_not_authoritative_closure() {
    let mut h = two_terminals();
    assert_eq!(close(&mut h, 2, 9), PhuxClientResult::Ok);
    acknowledge(&mut h, 2);
    release_terminal(&mut h.0.inner, &ResourceId::local(9)).unwrap();
    assert!(h.0.inner.operations.completed.is_empty());
    assert_eq!(h.0.inner.operations.pending.len(), 1);
    // SAFETY: harness exclusively owns this Client.
    unsafe {
        assert_eq!(phux_client_disconnect(h.ptr()), PhuxClientResult::Ok);
    }
    assert_eq!(result(&mut h, 0).status, 3);
}

fn result(h: &mut Harness, index: usize) -> PhuxOperationResult {
    let mut out = PhuxOperationResult::default();
    // SAFETY: harness owns client and writable output.
    unsafe {
        assert_eq!(
            phux_client_operation_get(h.ptr(), index, &raw mut out),
            PhuxClientResult::Ok
        );
    }
    out
}

#[test]
fn explicit_close_requires_live_owned_attachment_and_keeps_state_until_server_closure() {
    let mut h = Harness::new();
    h.negotiate();
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::InvalidState);
    assert!(h.0.inner.outgoing.is_empty());

    let mut h = Harness::attached();
    assert_eq!(close(&mut h, 1, 0), PhuxClientResult::InvalidArgument);
    assert_eq!(close(&mut h, 1, 999), PhuxClientResult::InvalidState);
    assert!(h.0.inner.outgoing.is_empty());
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::Ok);
    assert_eq!(
        FrameKind::decode(&h.0.inner.outgoing[0]).unwrap().0,
        FrameKind::Command {
            request_id: 1,
            command: Command::KillResource {
                terminal_id: ResourceId::local(1)
            }
        }
    );
    let id = ResourceId::local(1);
    assert!(h.0.inner.session.published(&id).is_some());
    assert!(h.0.inner.session.active_attach_contains(&id));
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::InvalidArgument);
    assert_eq!(close(&mut h, 2, 1), PhuxClientResult::InvalidState);
    assert_eq!(h.0.inner.outgoing.len(), 1);
    h.0.inner.outgoing.clear();
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    // The server's Ok confirms actor cancellation, not the later reap. This
    // assertion failed on 71730635: a success receipt was already exposed.
    assert!(h.0.inner.operations.completed.is_empty());
    assert!(
        h.0.inner.session.published(&id).is_some(),
        "receipt does not fake detach/ended"
    );
    assert_eq!(
        h.feed(FrameKind::ResourceClosed {
            terminal_id: id,
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(close(&mut h, 2, 1), PhuxClientResult::InvalidState);
    let out = result(&mut h, 0);
    assert_eq!((out.kind, out.status, out.terminal_id.id), (5, 1, 1));
}

#[test]
fn explicit_close_preserves_authoritative_refusal_and_does_not_reuse_request_ids() {
    let mut h = Harness::attached();
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::Ok);
    h.0.inner.outgoing.clear();
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message: "instance changed".into()
            }
        }),
        PhuxClientResult::Ok
    );
    let out = result(&mut h, 0);
    assert_eq!(
        (out.kind, out.status, out.error_domain, out.error_code),
        (5, 2, 2, u32::from(ErrorCode::PreconditionFailed.as_wire()))
    );
    // SAFETY: borrowed completion message remains valid until next mutation.
    assert_eq!(
        unsafe { bytes_in(out.message.data, out.message.len) }.unwrap(),
        b"instance changed"
    );
    assert!(h.0.inner.session.published(&ResourceId::local(1)).is_some());
    // SAFETY: harness owns client.
    unsafe {
        assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
    }
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::InvalidArgument);
    assert_eq!(close(&mut h, 2, 1), PhuxClientResult::Ok);
    h.0.inner.outgoing.clear();
    assert_eq!(
        h.feed(FrameKind::Error {
            request_id: Some(2),
            code: ErrorCode::PreconditionFailed,
            message: "authoritative refusal".into()
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(result(&mut h, 0).status, 2);
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Ok
        }),
        PhuxClientResult::ProtocolError
    );
}

#[test]
fn explicit_close_never_survives_connection_loss_before_send() {
    let mut captured = Harness::attached();
    assert_eq!(close(&mut captured, 1, 1), PhuxClientResult::Ok);
    // SAFETY: harness owns original connection-scoped client.
    unsafe {
        assert_eq!(phux_client_disconnect(captured.ptr()), PhuxClientResult::Ok);
    }
    assert!(captured.0.inner.outgoing.is_empty());
    let out = result(&mut captured, 0);
    assert_eq!((out.kind, out.status, out.terminal_id.id), (5, 3, 1));
    let replacement = Harness::attached(); // Numeric ID 1 is now a different incarnation.
    assert_eq!(close(&mut captured, 2, 1), PhuxClientResult::InvalidState);
    assert!(replacement.0.inner.outgoing.is_empty());
    assert!(
        replacement
            .0
            .inner
            .session
            .published(&ResourceId::local(1))
            .is_some()
    );
}

#[test]
fn explicit_close_uses_retained_instance_without_abandonment_conditions() {
    let mut h = Harness::attached();
    h.0.inner.conditional_kill = true;
    let options = PhuxSpawnOptions {
        request_id: 1,
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: harness owns client and options.
    unsafe {
        assert_eq!(
            phux_client_queue_spawn_bound(h.ptr(), &raw const options),
            PhuxClientResult::Ok
        );
    }
    h.0.inner.outgoing.clear();
    let token = ServerInstance::new([7; 16]);
    assert_eq!(
        h.feed(FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::OkBound {
                id: ResourceId::local(9),
                instance: token
            }
        }),
        PhuxClientResult::Ok
    );
    h.bootstrap(ResourceId::local(9));
    h.0.inner.outgoing.clear();
    // SAFETY: harness owns client; clearing the receipt must retain evidence.
    unsafe {
        assert_eq!(phux_client_operation_clear(h.ptr()), PhuxClientResult::Ok);
    }
    assert_eq!(close(&mut h, 2, 9), PhuxClientResult::Ok);
    assert_eq!(
        FrameKind::decode(&h.0.inner.outgoing[0]).unwrap().0,
        FrameKind::Command {
            request_id: 2,
            command: Command::KillResourceIf {
                terminal_id: ResourceId::local(9),
                precondition: KillPrecondition {
                    instance: Some(token),
                    conditions: phux_protocol::wire::frame::KillConditions::NONE
                }
            }
        }
    );
}

#[test]
fn explicit_close_rejects_pending_attach_detach_and_queue_overflow() {
    let mut h = Harness::attached();
    assert_eq!(h.attach(1, &ResourceId::local(9)), PhuxClientResult::Ok);
    h.bootstrap(ResourceId::local(9));
    assert_eq!(close(&mut h, 2, 9), PhuxClientResult::InvalidState);
    assert_eq!(h.detach(2, &ResourceId::local(1)), PhuxClientResult::Ok);
    assert_eq!(close(&mut h, 3, 1), PhuxClientResult::InvalidState);

    let mut h = Harness::attached();
    h.0.inner.outgoing.resize(MAX_OPERATIONS, Vec::new());
    assert_eq!(close(&mut h, 1, 1), PhuxClientResult::InvalidState);
    h.0.inner.outgoing.clear();
    assert_eq!(
        close(&mut h, 1, 1),
        PhuxClientResult::Ok,
        "local failure did not consume ID"
    );
}
