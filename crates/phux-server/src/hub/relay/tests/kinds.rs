//! Agent resume cursors and the relayed resource-kind catalog.

use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "one wire scenario table covers the capability and shared-generation matrix"
)]
#[test]
fn agent_resume_relay_routes_cursors_only_with_support_and_a_sole_consumer() {
    for (supports_resume, shared, cursor) in [
        (false, false, Some(1)),
        (true, false, Some(1)),
        (true, true, Some(1)),
        (true, false, None),
    ] {
        let limits = BootstrapLimits::default();
        let mut session = RelaySession::new_negotiated(
            host(),
            limits,
            BootstrapProfile::SynthesizedVtRaw,
            ServerFeatureSet::with(&[ServerFeature::ResourceKinds]),
        );
        if supports_resume {
            session.set_satellite_features_ext(ServerFeatureExtSet::with(&[
                ServerFeatureExt::AgentStreamResume,
            ]));
        }
        let (first_tx, mut first_rx) = mpsc::channel(16);
        if shared {
            attach(&mut session, 7, ClientId(2), first_tx);
        }
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let (reply, _result) = oneshot::channel();
        let wire = session.handle_request(RelayRequest::Command {
            command: Command::attach_agent_session(ResourceId::local(7), None, cursor),
            reply,
            subscribe: Some(ProxySubscription {
                resource_kind: Some(phux_protocol::ids::ResourceKind::AgentSession),
                terminal: 7,
                client: ClientId(1),
                out_tx,
                consumer_cancel: CancellationToken::new(),
                seq: 1,
                awaits_snapshot: true,
                bootstrap_profile: Some(native_profile()),
                bootstrap_limits: Some(limits),
            }),
        });
        let resume = cursor.is_some() && supports_resume && !shared;
        let FrameKind::Command {
            command: Command::AttachResource {
                agent_after_seq, ..
            },
            ..
        } = FrameKind::decode(&wire).expect("attach frame").0
        else {
            panic!("relay did not encode ATTACH_RESOURCE");
        };
        assert_eq!(agent_after_seq, resume.then_some(1));
        let profile = if resume {
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 1 }
        } else {
            BootstrapStreamProfile::AgentEventsJsonlV1
        };
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(2).expect("generation");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7), stream_id, bootstrap_id,
                profile, cols: 0, rows: 0, base_seq: 3,
            },
            FrameKind::BootstrapChunk {
                terminal_id: ResourceId::local(7), stream_id, bootstrap_id, chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"{\"seq\":2,\"ts_ms\":1,\"type\":\"ask\"}\n{\"seq\":3,\"ts_ms\":1,\"type\":\"stop\"}\n"),
            },
            FrameKind::BootstrapReady {
                terminal_id: ResourceId::local(7), stream_id, bootstrap_id, history_cursor: None,
            },
            FrameKind::ResourceOutput {
                terminal_id: ResourceId::local(7), stream_id, bootstrap_id, seq: 4,
                bytes: bytes::Bytes::from_static(b"{\"seq\":4,\"ts_ms\":1,\"type\":\"prompt\"}\n"),
            },
        ] {
            session.handle_inbound(&encode(&frame)).expect("valid agent stream");
        }
        let Outbound::Frame(FrameKind::BootstrapBegin {
            profile: observed, ..
        }) = out_rx.try_recv().expect("BEGIN")
        else {
            panic!("BEGIN")
        };
        assert_eq!(observed, profile);
        assert!(matches!(
            out_rx.try_recv().expect("CHUNK"),
            Outbound::Frame(FrameKind::BootstrapChunk { .. })
        ));
        assert!(matches!(
            out_rx.try_recv().expect("READY"),
            Outbound::Frame(FrameKind::BootstrapReady { .. })
        ));
        assert!(matches!(
            out_rx.try_recv().expect("live"),
            Outbound::Frame(FrameKind::ResourceOutput { seq: 4, .. })
        ));
        if shared {
            assert!(matches!(
                first_rx.try_recv().expect("shared BEGIN"),
                Outbound::Frame(FrameKind::BootstrapBegin {
                    profile: BootstrapStreamProfile::AgentEventsJsonlV1,
                    ..
                })
            ));
        }
        session
            .handle_inbound(&encode(&FrameKind::BootstrapTombstone {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::ExplicitReattach,
                last_valid_seq: 4,
            }))
            .expect("agent generation can be retired");
        session
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
                stream_id,
                bootstrap_id: BootstrapId::new(3).expect("replacement"),
                profile,
                cols: 0,
                rows: 0,
                base_seq: 4,
            }))
            .expect("a fresh agent generation follows the tombstone");
    }
}

#[test]
fn agent_resume_relay_rejects_unadvertised_resume_profiles_and_retags_the_cursor() {
    let routed = route_to_satellite(&Command::attach_agent_session(
        ResourceId::satellite(host(), 7),
        None,
        Some(42),
    ))
    .expect("satellite route");
    assert_eq!(
        routed.1,
        Command::attach_agent_session(ResourceId::local(7), None, Some(42)),
    );
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        BootstrapProfile::SynthesizedVtRaw,
        ServerFeatureSet::with(&[ServerFeature::ResourceKinds]),
    );
    assert!(
        session
            .handle_inbound(&encode(&FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(7),
                stream_id: StreamId::new(1).expect("stream"),
                bootstrap_id: BootstrapId::new(1).expect("generation"),
                profile: BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 0 },
                cols: 0,
                rows: 0,
                base_seq: 0,
            }))
            .is_err()
    );
}

pub(super) fn kind_snapshot() -> phux_protocol::wire::info::SessionSnapshot {
    use phux_protocol::ids::{ResourceKind, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(8)).with_resources(
        vec![
            ResourceInfo::resource(ResourceId::local(7), ResourceKind::AgentSession),
            ResourceInfo::new(ResourceId::local(8), WindowId::new(1), 80, 24),
        ],
    )
}

#[tokio::test]
async fn relay_kind_lookup_uses_one_bounded_inventory_then_only_learned_facts() {
    use phux_protocol::ids::ResourceKind;
    let (relay, mut mailbox) = RelayHandle::new(host());
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    assert!(relay.needs_kind_for_profile(native_profile()));
    assert!(!relay.needs_kind_for_profile(BootstrapProfile::SynthesizedVtRaw));
    let resolve = async {
        let request = mailbox
            .requests
            .recv()
            .await
            .expect("one inventory request");
        let wire = session.handle_request(request);
        let FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        } = decode(&wire)
        else {
            panic!("kind fallback must use the existing bounded server inventory");
        };
        session
            .handle_inbound(&encode(&FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::State(kind_snapshot())),
            }))
            .expect("inventory response");
    };
    let (kind, ()) = tokio::join!(relay.resource_kind(7), resolve);
    assert_eq!(kind.expect("agent kind"), ResourceKind::AgentSession);
    assert_eq!(
        relay.resource_kind(7).await.expect("cached agent"),
        ResourceKind::AgentSession
    );
    assert_eq!(
        relay.resource_kind(8).await.expect("cached terminal"),
        ResourceKind::Terminal
    );
    assert!(
        mailbox.requests.try_recv().is_err(),
        "enumeration must not refetch per resource"
    );

    session
        .handle_inbound(&encode(&FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(7),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Exited,
            signal: None,
        }))
        .expect("close invalidates the learned kind");
    assert_eq!(relay.known_resource_kind(7), None);
    assert_eq!(relay.known_resource_kind(8), Some(ResourceKind::Terminal));
    session.teardown("disconnected");
    assert_eq!(relay.known_resource_kind(8), None);
    assert!(!relay.needs_kind_for_profile(native_profile()));
}

#[test]
fn relay_kind_facts_follow_spawn_events_replies_and_connection_incarnations() {
    use phux_protocol::ids::ResourceKind;
    let (relay, mailbox) = RelayHandle::new(host());
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    session.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    session
        .handle_inbound(&encode(&FrameKind::Event {
            terminal: Some(ResourceId::local(9)),
            event: AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: None,
            },
            stamp: None,
        }))
        .expect("spawn event");
    assert_eq!(
        relay.known_resource_kind(9),
        Some(ResourceKind::AgentSession)
    );

    let (reply, mut result) = oneshot::channel();
    let mut request = spawn_request(reply);
    if let RelayRequest::Spawn { spawn, .. } = &mut request {
        spawn.resource = Some(Box::new(
            phux_protocol::wire::frame::SpawnResource::agent_session(ResourceId::local(8), "test"),
        ));
    }
    let FrameKind::SpawnResource { request_id, .. } = decode(&session.handle_request(request))
    else {
        panic!("spawn request")
    };
    session
        .handle_inbound(&encode(&FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(ResourceId::local(10)),
        }))
        .expect("spawn response");
    assert!(matches!(
        result.try_recv().expect("spawn result"),
        SpawnResult::Ok(_)
    ));
    assert_eq!(
        relay.known_resource_kind(10),
        Some(ResourceKind::AgentSession)
    );

    let mut replacement = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ResourceKinds]),
    );
    replacement.set_resource_kinds(Arc::clone(&mailbox.resource_kinds));
    assert_eq!(relay.known_resource_kind(9), None);
    assert_eq!(relay.known_resource_kind(10), None);
    assert!(!relay.needs_kind_for_profile(native_profile()));
    assert!(relay.needs_kind_for_profile(BootstrapProfile::SynthesizedVtRaw));
}
