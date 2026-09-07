use super::*;
use crate::*;
use phux_protocol::wire::frame::ErrorCode;
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, PROTOCOL_VERSION,
    StreamId,
};

struct Harness(Box<PhuxClient>);

impl Harness {
    fn new() -> Self {
        let limits = crate::client::Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        };
        Self(Box::new(PhuxClient {
            inner: Client::new(limits),
            _not_send_sync: std::marker::PhantomData,
        }))
    }

    fn ptr(&mut self) -> *mut PhuxClient {
        ptr::from_mut(self.0.as_mut())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test harness consumes temporary frame fixtures"
    )]
    fn feed(&mut self, frame: FrameKind) -> PhuxClientResult {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        // SAFETY: harness owns the live client and encoded span.
        unsafe { phux_client_feed_frame(self.ptr(), encoded.as_ptr(), encoded.len()) }
    }

    fn negotiate(&mut self) {
        // SAFETY: harness owns client; literal name is readable.
        assert_eq!(
            unsafe { phux_client_queue_hello(self.ptr(), bytes_out(b"test")) },
            PhuxClientResult::Ok
        );
        assert_eq!(
            self.feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: phux_protocol::caps::ServerCapabilities::new(),
                server_id: b"incarnation\0opaque".to_vec(),
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("limits"),
            }),
            PhuxClientResult::Ok
        );
        self.0.inner.outgoing.clear();
    }

    fn attached() -> Self {
        let mut h = Self::new();
        h.negotiate();
        // Exercise the real attach barrier with one inventory terminal.
        let options = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols: 40,
            rows: 12,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        // SAFETY: client and options are owned by the harness.
        assert_eq!(
            unsafe { phux_client_queue_attach(h.ptr(), &raw const options) },
            PhuxClientResult::Ok
        );
        let session = phux_protocol::SessionId::new(1);
        let window = phux_protocol::WindowId::new(1);
        let snapshot =
            phux_protocol::wire::info::SessionSnapshot::new(session, window, TerminalId::local(1))
                .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
                    window, session, "main",
                )])
                .with_panes(vec![phux_protocol::wire::info::TerminalInfo::new(
                    TerminalId::local(1),
                    window,
                    40,
                    12,
                )]);
        assert_eq!(
            h.feed(FrameKind::Attached {
                attach_id: 1,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(1)
            }),
            PhuxClientResult::Ok
        );
        h.bootstrap(TerminalId::local(1));
        assert_eq!(
            h.feed(FrameKind::AttachReady { attach_id: 1 }),
            PhuxClientResult::Ok
        );
        h.0.inner.outgoing.clear();
        h.0.inner.owned_effects.clear();
        h
    }

    fn spawn(&mut self, request_id: u32) -> PhuxClientResult {
        // SAFETY: harness owns client and temporary options for this call.
        unsafe {
            phux_client_queue_spawn(
                self.ptr(),
                &PhuxSpawnOptions {
                    request_id,
                    ..PhuxSpawnOptions::default()
                },
            )
        }
    }

    fn attach(&mut self, request_id: u32, id: &TerminalId) -> PhuxClientResult {
        // SAFETY: borrowed ID host remains live for this call.
        unsafe {
            phux_client_queue_attach_terminal(
                self.ptr(),
                &PhuxAttachTerminalOptions {
                    request_id,
                    terminal_id: terminal_id_out(id),
                    ..PhuxAttachTerminalOptions::default()
                },
            )
        }
    }

    fn bootstrap(&mut self, terminal_id: TerminalId) {
        let stream_id = StreamId::new(17).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 40,
                rows: 12,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"dynamic terminal"),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            assert_eq!(self.feed(frame), PhuxClientResult::Ok);
        }
    }

    fn detach(&mut self, request_id: u32, id: &TerminalId) -> PhuxClientResult {
        // SAFETY: harness owns the client and borrowed options/host.
        unsafe {
            phux_client_queue_detach_terminal(
                self.ptr(),
                &PhuxDetachTerminalOptions {
                    request_id,
                    terminal_id: terminal_id_out(id),
                    ..PhuxDetachTerminalOptions::default()
                },
            )
        }
    }

    fn result(&mut self, index: usize) -> PhuxOperationResult {
        let mut out = PhuxOperationResult::default();
        // SAFETY: output is writable and disjoint from the client.
        assert_eq!(
            unsafe { phux_client_operation_get(self.ptr(), index, &raw mut out) },
            PhuxClientResult::Ok
        );
        out
    }
}

#[test]
fn detach_retires_initial_participation_only_on_success_and_allows_explicit_reattach() {
    let mut h = Harness::attached();
    let id = TerminalId::local(1);
    assert_eq!(h.detach(1, &id), PhuxClientResult::Ok);
    assert!(h.0.inner.session.published(&id).is_some());
    assert_eq!(h.detach(2, &id), PhuxClientResult::InvalidState);
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    assert!(h.0.inner.session.published(&id).is_none());
    assert!(!h.0.inner.session.active_attach_contains(&id));
    assert_eq!(h.result(0).kind, 3);
    assert_eq!(h.attach(2, &id), PhuxClientResult::Ok);
    h.bootstrap(id.clone());
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 2,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    assert!(h.0.inner.session.published(&id).is_some());
}

#[test]
fn detach_refusal_preserves_replica_and_disconnect_is_unknown_without_replay() {
    let mut h = Harness::attached();
    let id = TerminalId::local(1);
    assert_eq!(h.detach(1, &id), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::Error {
            request_id: Some(1),
            code: ErrorCode::InvalidCommand,
            message: "refused".into()
        }),
        PhuxClientResult::Ok
    );
    assert!(h.0.inner.session.published(&id).is_some());
    assert_eq!(h.result(0).status, 2);
    assert_eq!(h.detach(2, &id), PhuxClientResult::Ok);
    // SAFETY: harness owns client.
    assert_eq!(
        unsafe { phux_client_disconnect(h.ptr()) },
        PhuxClientResult::Ok
    );
    assert_eq!(h.result(1).status, 3);
    assert!(h.0.inner.outgoing.is_empty());
}

#[test]
fn detach_is_available_at_full_dynamic_admission_capacity() {
    let mut h = Harness::attached();
    for id in 2..=u32::try_from(MAX_DYNAMIC_TERMINALS + 1).unwrap() {
        h.0.inner.operations.dynamic.insert(TerminalId::local(id));
    }
    assert_eq!(h.spawn(1), PhuxClientResult::InvalidState);
    assert_eq!(h.detach(1, &TerminalId::local(2)), PhuxClientResult::Ok);
}

#[test]
fn detached_stream_is_unsolicited_and_wrong_reply_does_not_consume_detach() {
    let mut h = Harness::attached();
    let id = TerminalId::local(1);
    assert_eq!(h.detach(1, &id), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::TerminalSpawned {
            request_id: 1,
            result: SpawnResult::Ok(TerminalId::local(8))
        }),
        PhuxClientResult::ProtocolError
    );
    assert!(h.0.inner.session.published(&id).is_some());
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(
        h.feed(FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id: StreamId::new(18).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        }),
        PhuxClientResult::ProtocolError
    );
    assert!(!h.0.inner.operations.admitted(&id));
    assert_eq!(h.result(0).status, 1);
}

#[test]
fn local_spawn_is_correlated_and_admitted_before_bootstrap_without_restarting_attach() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(10), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Ok(TerminalId::local(2))
        }),
        PhuxClientResult::Ok
    );
    let result = h.result(0);
    assert_eq!(
        (
            result.request_id,
            result.kind,
            result.status,
            result.terminal_id.id
        ),
        (10, 1, 1, 2)
    );
    assert!(
        h.0.inner
            .session
            .active_attach_contains(&TerminalId::local(1))
    );
    assert!(
        !h.0.inner
            .session
            .active_attach_contains(&TerminalId::local(2))
    );
    h.bootstrap(TerminalId::local(2));
    assert!(h.0.inner.session.published(&TerminalId::local(2)).is_some());
    assert_eq!(
        h.attach(11, &TerminalId::local(2)),
        PhuxClientResult::InvalidState
    );
    assert!(
        h.0.inner
            .ensure_participant(&TerminalId::local(99))
            .is_err()
    );
}

#[test]
fn satellite_spawn_requires_explicit_attach_and_bootstrap_can_precede_ack() {
    let mut h = Harness::attached();
    let options = PhuxSpawnOptions {
        request_id: 10,
        satellite: bytes_out(b"remote"),
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: all pointers are owned by the harness or static literals.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::Ok
    );
    let id = TerminalId::satellite(SatelliteHost::new("remote"), 22);
    assert_eq!(
        h.feed(FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Ok(id.clone())
        }),
        PhuxClientResult::Ok
    );
    assert!(!h.0.inner.operations.admitted(&id));
    assert_eq!(h.attach(11, &id), PhuxClientResult::Ok);
    h.bootstrap(id.clone());
    assert!(h.0.inner.session.published(&id).is_some());
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 11,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    let result = h.result(1);
    assert_eq!(
        (
            result.kind,
            result.status,
            result.terminal_id.kind,
            result.terminal_id.id
        ),
        (2, 1, 1, 22)
    );
    // SAFETY: result's host is borrowed without intervening mutation.
    assert_eq!(
        unsafe { bytes_in(result.terminal_id.host.data, result.terminal_id.host.len) }
            .expect("host"),
        b"remote"
    );
}

#[test]
fn refusals_are_scoped_results_and_revoke_only_the_requested_admission() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(10), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Err(SpawnError::SpawnFailed("no PTY".into()))
        }),
        PhuxClientResult::Ok
    );
    assert_eq!((h.result(0).status, h.result(0).error_domain), (2, 1));
    assert!(h.0.inner.owned_effects.is_empty());
    assert_eq!(h.attach(11, &TerminalId::local(2)), PhuxClientResult::Ok);
    h.bootstrap(TerminalId::local(2));
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 11,
            result: CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "refused".into()
            }
        }),
        PhuxClientResult::Ok
    );
    assert!(!h.0.inner.operations.admitted(&TerminalId::local(2)));
    assert!(h.0.inner.session.published(&TerminalId::local(2)).is_none());
    assert!(h.0.inner.session.published(&TerminalId::local(1)).is_some());
    assert!(
        !h.0.inner
            .owned_effects
            .iter()
            .any(|effect| effect.kind == 2 && effect.detail == 4)
    );
    assert_eq!(h.spawn(12), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::Error {
            request_id: Some(12),
            code: ErrorCode::InvalidCommand,
            message: "spawn denied".into()
        }),
        PhuxClientResult::Ok
    );
    assert_eq!((h.result(2).request_id, h.result(2).error_domain), (12, 2));
}

#[test]
fn malformed_duplicate_and_wrong_kind_results_do_not_consume_pending_requests() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(10), PhuxClientResult::Ok);
    for frame in [
        FrameKind::CommandResult {
            request_id: 10,
            result: CommandResult::Ok,
        },
        FrameKind::TerminalSpawned {
            request_id: 11,
            result: SpawnResult::Ok(TerminalId::local(2)),
        },
        FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Ok(TerminalId::local(0)),
        },
        FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Ok(TerminalId::local(1)),
        },
        FrameKind::TerminalSpawned {
            request_id: 10,
            result: SpawnResult::Ok(TerminalId::satellite(SatelliteHost::new("other"), 2)),
        },
    ] {
        assert_eq!(h.feed(frame), PhuxClientResult::ProtocolError);
    }
    assert_eq!(h.0.inner.operations.pending.len(), 1);
    let ok = FrameKind::TerminalSpawned {
        request_id: 10,
        result: SpawnResult::Ok(TerminalId::local(2)),
    };
    assert_eq!(h.feed(ok.clone()), PhuxClientResult::Ok);
    assert_eq!(h.feed(ok), PhuxClientResult::ProtocolError);
    // SAFETY: harness exclusively owns client.
    assert_eq!(
        unsafe { phux_client_operation_clear(h.ptr()) },
        PhuxClientResult::Ok
    );
    assert_eq!(h.spawn(10), PhuxClientResult::InvalidArgument);
}

#[test]
fn disconnect_cancels_pending_without_retry_and_retains_opaque_server_identity() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(10), PhuxClientResult::Ok);
    assert_eq!(h.attach(11, &TerminalId::local(2)), PhuxClientResult::Ok);
    // SAFETY: harness exclusively owns client.
    assert_eq!(
        unsafe { phux_client_disconnect(h.ptr()) },
        PhuxClientResult::Ok
    );
    assert!(h.0.inner.outgoing.is_empty());
    assert!(h.0.inner.operations.pending.is_empty());
    assert!(h.0.inner.operations.dynamic.is_empty());
    assert_eq!((h.result(0).request_id, h.result(0).status), (10, 3));
    assert_eq!(
        (
            h.result(1).request_id,
            h.result(1).status,
            h.result(1).terminal_id.id
        ),
        (11, 3, 2)
    );
    assert_eq!(h.spawn(12), PhuxClientResult::InvalidState);
    let mut identity = PhuxBytes::default();
    // SAFETY: readable client and disjoint writable output; borrowed bytes stay live.
    unsafe {
        assert_eq!(phux_client_disconnect(h.ptr()), PhuxClientResult::Ok);
        assert_eq!(phux_client_operation_count(h.ptr()), 2);
        assert_eq!(
            phux_client_server_id(h.ptr(), &raw mut identity),
            PhuxClientResult::Ok
        );
        assert_eq!(
            bytes_in(identity.data, identity.len).expect("identity"),
            b"incarnation\0opaque"
        );
    }
}

#[test]
fn pending_plus_results_and_dynamic_admissions_are_bounded() {
    let mut h = Harness::attached();
    for request_id in 1..=128 {
        assert_eq!(h.spawn(request_id), PhuxClientResult::Ok);
    }
    assert_eq!(h.spawn(129), PhuxClientResult::InvalidState);
    assert_eq!(
        h.feed(FrameKind::TerminalSpawned {
            request_id: 1,
            result: SpawnResult::Err(SpawnError::GroupNotFound)
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(h.spawn(129), PhuxClientResult::InvalidState);
    // SAFETY: harness exclusively owns client.
    assert_eq!(
        unsafe { phux_client_operation_clear(h.ptr()) },
        PhuxClientResult::Ok
    );
    h.0.inner.outgoing.clear();
    assert_eq!(h.spawn(129), PhuxClientResult::Ok);

    let mut h = Harness::attached();
    for n in 2..=257 {
        assert_eq!(h.attach(n, &TerminalId::local(n)), PhuxClientResult::Ok);
        assert_eq!(
            h.feed(FrameKind::CommandResult {
                request_id: n,
                result: CommandResult::Ok
            }),
            PhuxClientResult::Ok
        );
        // SAFETY: harness exclusively owns client.
        assert_eq!(
            unsafe { phux_client_operation_clear(h.ptr()) },
            PhuxClientResult::Ok
        );
        h.0.inner.outgoing.clear();
    }
    assert_eq!(
        h.attach(258, &TerminalId::local(258)),
        PhuxClientResult::InvalidState
    );
    assert_eq!(h.spawn(258), PhuxClientResult::InvalidState);
    assert_eq!(
        h.feed(FrameKind::TerminalClosed {
            terminal_id: TerminalId::local(2),
            exit_status: None
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(h.spawn(258), PhuxClientResult::Ok);
}

#[test]
fn spawn_options_are_encoded_exactly_and_validation_is_transactional() {
    let mut h = Harness::attached();
    let owner = terminal_id_out(&TerminalId::local(1));
    let argv = [
        bytes_out(b"/bin/sh"),
        bytes_out(b"-c"),
        bytes_out(b"echo hello"),
    ];
    let options = PhuxSpawnOptions {
        request_id: 7,
        owner_terminal: &raw const owner,
        argv: argv.as_ptr(),
        argc: argv.len(),
        cwd: bytes_out(b"/workspace"),
        cols: 90,
        rows: 31,
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: harness owns all input records and spans.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::Ok
    );
    let (frame, remaining) = FrameKind::decode(&h.0.inner.outgoing[0]).expect("decode");
    assert!(remaining.is_empty());
    assert!(
        matches!(frame, FrameKind::SpawnTerminal { request_id: 7, owner_terminal: Some(TerminalId::Local { id: 1 }), initial_size: Some((90, 31)), command: Some(ref command), cwd: Some(ref cwd), .. }
        if command == &["/bin/sh", "-c", "echo hello"] && cwd == "/workspace")
    );
    for bad in [
        PhuxSpawnOptions {
            request_id: 8,
            version: 2,
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            size: 0,
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            cols: 0,
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            rows: 0,
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            argc: MAX_SPAWN_ARGS + 1,
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            argv: ptr::null(),
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            satellite: bytes_out(b"remote"),
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            cwd: bytes_out(b"bad\0cwd"),
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            cwd: bytes_out(b"\xff"),
            ..options
        },
        PhuxSpawnOptions {
            request_id: 8,
            cwd: PhuxBytes {
                data: ptr::null(),
                len: MAX_SPAWN_BYTES + 1,
            },
            ..options
        },
    ] {
        // SAFETY: spans are readable except deliberately null spans, rejected before read.
        assert_eq!(
            unsafe { phux_client_queue_spawn(h.ptr(), &raw const bad) },
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(h.0.inner.outgoing.len(), 1);
    }
    assert_eq!(h.spawn(8), PhuxClientResult::Ok);
}

#[test]
fn pre_attach_operations_are_rejected_and_result_output_is_sized() {
    let mut h = Harness::new();
    assert_eq!(h.spawn(1), PhuxClientResult::InvalidState);
    h.negotiate();
    assert_eq!(h.spawn(1), PhuxClientResult::InvalidState);
    assert_eq!(
        h.attach(1, &TerminalId::local(2)),
        PhuxClientResult::InvalidState
    );
    let mut result = PhuxOperationResult {
        request_id: 123,
        ..PhuxOperationResult::default()
    };
    // SAFETY: harness owns client and disjoint result.
    unsafe {
        assert_eq!(
            phux_client_operation_get(h.ptr(), 0, &raw mut result),
            PhuxClientResult::NoValue
        );
        assert_eq!(result.request_id, 0);
        result.size = 0;
        assert_eq!(
            phux_client_operation_get(h.ptr(), 0, &raw mut result),
            PhuxClientResult::InvalidArgument
        );
    }
}

#[test]
fn refusal_messages_are_bounded_without_splitting_utf8() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(1), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::Error {
            request_id: Some(1),
            code: ErrorCode::InvalidCommand,
            message: "€".repeat(2000)
        }),
        PhuxClientResult::Ok
    );
    let result = h.result(0);
    assert!(result.message.len <= MAX_OPERATION_MESSAGE_BYTES);
    // SAFETY: no mutation since borrowing the result.
    assert!(
        std::str::from_utf8(
            unsafe { bytes_in(result.message.data, result.message.len) }.expect("span")
        )
        .is_ok()
    );
}

#[test]
fn completed_results_cannot_hide_an_undrained_outgoing_operation_queue() {
    let mut h = Harness::attached();
    for request_id in 1..=128 {
        assert_eq!(h.spawn(request_id), PhuxClientResult::Ok);
        assert_eq!(
            h.feed(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::GroupNotFound)
            }),
            PhuxClientResult::Ok
        );
        // SAFETY: harness exclusively owns client.
        assert_eq!(
            unsafe { phux_client_operation_clear(h.ptr()) },
            PhuxClientResult::Ok
        );
    }
    assert_eq!(h.spawn(129), PhuxClientResult::InvalidState);
    assert_eq!(h.0.inner.outgoing.len(), MAX_OPERATIONS);
}

#[test]
fn refused_attach_can_be_explicitly_retried_with_or_without_provisional_bootstrap() {
    for provisional in [false, true] {
        let mut h = Harness::attached();
        let id = TerminalId::satellite(SatelliteHost::new("remote"), 22);
        assert_eq!(h.attach(10, &id), PhuxClientResult::Ok);
        if provisional {
            h.bootstrap(id.clone());
        }
        assert_eq!(
            h.feed(FrameKind::CommandResult {
                request_id: 10,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "route temporarily unavailable".into()
                }
            }),
            PhuxClientResult::Ok
        );
        assert!(!h.0.inner.operations.admitted(&id));
        assert_eq!(h.attach(11, &id), PhuxClientResult::Ok);
        h.bootstrap(id.clone());
        assert_eq!(
            h.feed(FrameKind::CommandResult {
                request_id: 11,
                result: CommandResult::Ok
            }),
            PhuxClientResult::Ok
        );
        assert!(h.0.inner.session.published(&id).is_some());
        assert!(h.0.inner.session.published(&TerminalId::local(1)).is_some());
    }
}

#[test]
fn dynamic_terminal_churn_releases_kernel_identity_retention() {
    use phux_client_core::session::{InputBlockReason, InputEligibility};
    let mut h = Harness::attached();
    for n in 2..=513 {
        let id = TerminalId::local(n);
        assert_eq!(h.attach(n, &id), PhuxClientResult::Ok);
        assert_eq!(
            h.feed(FrameKind::CommandResult {
                request_id: n,
                result: CommandResult::Ok
            }),
            PhuxClientResult::Ok
        );
        assert_eq!(
            h.feed(FrameKind::TerminalClosed {
                terminal_id: id.clone(),
                exit_status: None
            }),
            PhuxClientResult::Ok
        );
        assert_eq!(
            h.0.inner.session.input_eligibility(&id),
            InputEligibility::Ineligible(InputBlockReason::UnknownTerminal)
        );
        assert!(!h.0.inner.operations.admitted(&id));
        assert!(h.0.inner.ensure_participant(&id).is_err());
        // SAFETY: harness exclusively owns client.
        assert_eq!(
            unsafe { phux_client_operation_clear(h.ptr()) },
            PhuxClientResult::Ok
        );
        h.0.inner.outgoing.clear();
    }
}

#[test]
fn outbound_ids_and_aggregate_text_limit_are_validated_before_queue_mutation() {
    let mut h = Harness::attached();
    assert_eq!(h.spawn(0), PhuxClientResult::InvalidArgument);
    assert_eq!(
        h.attach(1, &TerminalId::local(0)),
        PhuxClientResult::InvalidArgument
    );
    let text = vec![b'x'; MAX_SPAWN_BYTES];
    let mut options = PhuxSpawnOptions {
        request_id: 1,
        cwd: bytes_out(&text),
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: harness owns all input storage.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::Ok
    );
    let argv = [bytes_out(b"sh")];
    options.request_id = 2;
    options.argv = argv.as_ptr();
    options.argc = argv.len();
    // SAFETY: all spans are readable; aggregate length is deliberately oversized.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::InvalidArgument
    );
    let empty_program = [PhuxBytes::default()];
    options.argv = empty_program.as_ptr();
    options.cwd = PhuxBytes::default();
    // SAFETY: one readable empty argument, rejected as an executable.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(h.0.inner.outgoing.len(), 1);
    assert_eq!(h.spawn(u32::MAX), PhuxClientResult::Ok);
    assert_eq!(h.spawn(u32::MAX), PhuxClientResult::InvalidArgument);
    assert_eq!(h.spawn(1), PhuxClientResult::InvalidArgument);
}

#[test]
fn closure_before_attach_result_cannot_allow_overlapping_admission_owners() {
    let mut h = Harness::attached();
    let id = TerminalId::satellite(SatelliteHost::new("remote"), 22);
    assert_eq!(h.attach(10, &id), PhuxClientResult::Ok);
    assert_eq!(
        h.feed(FrameKind::TerminalClosed {
            terminal_id: id.clone(),
            exit_status: None
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(h.attach(11, &id), PhuxClientResult::InvalidState);
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 10,
            result: CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: "old attach failed".into()
            }
        }),
        PhuxClientResult::Ok
    );
    assert_eq!(h.attach(11, &id), PhuxClientResult::Ok);
    h.bootstrap(id.clone());
    assert_eq!(
        h.feed(FrameKind::CommandResult {
            request_id: 11,
            result: CommandResult::Ok
        }),
        PhuxClientResult::Ok
    );
    assert!(h.0.inner.operations.admitted(&id));
    assert!(h.0.inner.session.published(&id).is_some());
}

#[test]
fn satellite_owner_requires_exact_explicit_route_and_preserves_wire_identity() {
    for (owner, route, expected) in [
        (TerminalId::local(1), "", PhuxClientResult::Ok),
        (
            TerminalId::local(1),
            "remote",
            PhuxClientResult::InvalidArgument,
        ),
        (
            TerminalId::satellite(SatelliteHost::new("remote"), 22),
            "",
            PhuxClientResult::InvalidArgument,
        ),
        (
            TerminalId::satellite(SatelliteHost::new("remote"), 22),
            "other",
            PhuxClientResult::InvalidArgument,
        ),
        (
            TerminalId::satellite(SatelliteHost::new("remote"), 0),
            "remote",
            PhuxClientResult::InvalidArgument,
        ),
        (
            TerminalId::satellite(SatelliteHost::new("remote"), 22),
            "remote",
            PhuxClientResult::Ok,
        ),
    ] {
        let mut h = Harness::attached();
        let owner_view = terminal_id_out(&owner);
        let options = PhuxSpawnOptions {
            request_id: 2,
            owner_terminal: &raw const owner_view,
            satellite: bytes_out(route.as_bytes()),
            cols: 90,
            rows: 31,
            ..PhuxSpawnOptions::default()
        };
        // SAFETY: owner and route spans remain readable throughout the call.
        assert_eq!(
            unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
            expected
        );
        if expected == PhuxClientResult::InvalidArgument {
            assert!(h.0.inner.outgoing.is_empty());
            assert_eq!(h.spawn(2), PhuxClientResult::Ok);
            continue;
        }
        let (frame, remaining) =
            FrameKind::decode(&h.0.inner.outgoing[0]).expect("spawn wire frame");
        assert!(remaining.is_empty());
        let FrameKind::SpawnTerminal {
            owner_terminal,
            satellite,
            initial_size,
            ..
        } = frame
        else {
            panic!("expected spawn frame");
        };
        assert_eq!(owner_terminal.as_ref(), Some(&owner));
        assert_eq!(
            satellite
                .as_ref()
                .map(SatelliteHost::as_str)
                .unwrap_or_default(),
            route
        );
        assert_eq!(initial_size, Some((90, 31)));
    }
}

#[test]
fn satellite_owner_host_counts_toward_aggregate_spawn_text_bound() {
    let mut h = Harness::attached();
    let route = "h".repeat(MAX_SPAWN_BYTES / 2);
    let owner = TerminalId::satellite(SatelliteHost::new(&route), 22);
    let owner_view = terminal_id_out(&owner);
    let mut options = PhuxSpawnOptions {
        request_id: 2,
        owner_terminal: &raw const owner_view,
        satellite: bytes_out(route.as_bytes()),
        ..PhuxSpawnOptions::default()
    };
    // SAFETY: all inputs are readable, together exactly at the aggregate bound.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::Ok
    );
    options.request_id = 3;
    options.cwd = bytes_out(b"x");
    // SAFETY: all inputs are readable; their aggregate length deliberately exceeds the bound.
    assert_eq!(
        unsafe { phux_client_queue_spawn(h.ptr(), &raw const options) },
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(h.0.inner.outgoing.len(), 1);
    assert_eq!(h.spawn(3), PhuxClientResult::Ok);
}
