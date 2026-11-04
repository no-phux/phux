use super::*;
use phux_protocol::wire::frame::{FrameKind, RESOURCE_AGENT_KEY, RolePolicy, Scope};

#[test]
fn native_json_is_validated_before_scheduling() {
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt,
        ServerFeatureExtSet,
    };
    let mut remote = RemoteClient::new("unused".into(), 0, 0, None, None);
    let runtime =
        phux_client_runtime::Runtime::embedded(phux_client_runtime::control::ControlOptions {
            automatic_lifecycle: false,
            ..phux_client_runtime::control::ControlOptions::default()
        });
    *Arc::get_mut(&mut remote).unwrap().client.get_mut().unwrap() = Some(runtime.clone());
    runtime.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    drop(runtime.take_outbound());
    runtime
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::NativeAgent])),
            server_id: vec![0xAB; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    drop(runtime.take_outbound());
    for json in [
        r#"{"type":"prompt","text":" "}"#.to_owned(),
        r#"{"type":"interrupt","turn_id":""}"#.to_owned(),
        r#"{"type":"answer","question_id":"","response":{"confirmed":true}}"#.to_owned(),
        r#"{"type":"prompt","text":"ok","unexpected":true}"#.to_owned(),
        "x".repeat(phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES + 1),
    ] {
        assert_eq!(
            remote.native_agent_command("local:22".into(), "activation-A".into(), json),
            None
        );
    }
    assert_eq!(
        remote.native_agent_command(
            "not-an-id".into(),
            "activation-A".into(),
            r#"{"type":"prompt","text":"ok"}"#.into()
        ),
        None
    );
    assert!(runtime.take_outbound().is_empty());
}

/// A zero constructor viewport reaches the runtime as zero, the no-vote
/// contract, while an explicit `resize_viewport` still votes at least 1x1.
#[test]
fn a_zero_viewport_casts_no_vote_until_one_is_requested() {
    let remote = RemoteClient::new("unused".into(), 0, 0, None, None);
    let options = remote.control_options();
    assert_eq!(options.viewport, (0, 0));
    assert!(!phux_client_runtime::control::ControlPlane::new(options).votes_on_geometry());

    let sized = RemoteClient::new("unused".into(), 80, 24, None, None);
    assert_eq!(sized.control_options().viewport, (80, 24));

    remote.resize_viewport(0, 0);
    assert_eq!(remote.control_options().viewport, (1, 1));
}

/// The parity verbs before a connection exists refuse without panicking,
/// the viewer switch shapes the next attach, and a batch close reply
/// reaches the consumer by its request id.
#[test]
fn parity_verbs_refuse_offline_and_project_their_replies() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    assert_eq!(remote.attach_role(), None);
    remote.set_attach_viewer(true);
    assert_eq!(remote.attach_role(), Some(RolePolicy::VIEWER));

    assert_eq!(
        remote.resize_terminal("1".into(), 100, 40),
        WireResizeOutcome::Unavailable
    );
    assert_eq!(remote.close_terminals(vec!["1".into(), "2".into()]), 0);
    assert_eq!(
        remote.set_global_metadata("phux.push/v1/d".into(), b"{}".to_vec()),
        0
    );
    assert_eq!(remote.delete_global_metadata("phux.push/v1/d".into()), 0);
    assert!(matches!(
        remote.projection_selection_text("1".into()),
        Err(search::SearchError::Unavailable)
    ));
    assert!(matches!(
        remote.set_projection_selection("1".into(), 1, 2, false),
        Err(search::SearchError::Unavailable)
    ));

    let mut projected = Vec::new();
    remote.project_event(
        Event::TerminalsClosed {
            request_id: 9,
            error: Some("refused".into()),
        },
        &mut projected,
    );
    assert_eq!(
        projected,
        vec![WireEvent::TerminalsClosed {
            request_id: 9,
            error: Some("refused".into()),
        }]
    );
}

#[test]
fn routed_constructor_preserves_route_and_credentials_in_runtime_target() {
    for (url, transport) in [
        (
            "quic://relay.example:443",
            Transport::Quic("relay.example:443".into()),
        ),
        (
            "wss://relay.example:443/phux",
            Transport::Ws("wss://relay.example:443/phux".into()),
        ),
    ] {
        let remote = RemoteClient::new_routed(
            url.into(),
            80,
            24,
            Some("pin".into()),
            Some("token".into()),
            " mini-route.example ".into(),
        )
        .unwrap();
        let target = remote.target().unwrap();
        assert_eq!(target.transport, transport);
        assert_eq!(
            target.tls_server_name.as_deref(),
            Some("mini-route.example")
        );
        assert_eq!(target.cert_fingerprint.as_deref(), Some("pin"));
        assert_eq!(target.token.as_deref(), Some("token"));
        assert_eq!(target.token_file, None);
        assert_eq!(
            target.authority.route.as_deref(),
            Some("mini-route.example")
        );
        assert_eq!(target.authority.ca, None, "a relay leaf pins no server CA");
        assert_eq!(
            target.client_identity,
            phux_client_runtime::TlsClientIdentity::None
        );
        assert!(remote.runtime_client().is_none());

        let direct = RemoteClient::new(url.into(), 80, 24, None, None);
        assert_eq!(direct.target().unwrap().tls_server_name, None);

        remote.set_authority_pin(Some("server-ca".into()));
        remote.set_relay_route(Some("replacement-route".into()));
        let target = remote.target().unwrap();
        assert_eq!(target.authority.ca.as_deref(), Some("server-ca"));
        assert_eq!(target.authority.route.as_deref(), Some("replacement-route"));
        assert_eq!(target.tls_server_name.as_deref(), Some("replacement-route"));
    }
}

#[test]
fn routed_constructor_rejects_malformed_dns_before_starting_runtime() {
    for route in [
        "",
        " ",
        "127.0.0.1",
        "::1",
        "bad_route",
        "-route",
        "route-",
        "route..example",
        "route.example.",
        "route:443",
        "route/path",
        "route?x=y",
        "route#fragment",
        "user@route",
        "routé.example",
    ] {
        let result = RemoteClient::new_routed(
            "quic://relay.example:443".into(),
            80,
            24,
            None,
            None,
            route.into(),
        );
        assert!(
            matches!(result, Err(WireError::Runtime { reason }) if reason.contains("TLS server name")),
            "{route:?}"
        );
    }
}

#[test]
fn remote_endpoint_classification_selects_the_runtime_transport() {
    assert_eq!(
        classify_remote_endpoint("quic://mini.example:8788"),
        Ok(Transport::Quic("mini.example:8788".to_owned()))
    );
    assert_eq!(
        classify_remote_endpoint("quic://[fd7a:115c:a1e0::1]:8788"),
        Ok(Transport::Quic("[fd7a:115c:a1e0::1]:8788".to_owned()))
    );
    for url in ["ws://127.0.0.1:8787", "wss://mini.example:8787"] {
        assert_eq!(
            classify_remote_endpoint(url),
            Ok(Transport::Ws(url.to_owned()))
        );
    }
}

#[test]
fn remote_endpoint_classification_rejects_unsupported_or_malformed_targets() {
    for endpoint in [
        "https://mini.example:8788",
        "quic://mini.example",
        "quic://mini.example:not-a-port",
        "quic://fd7a:115c:a1e0::1:8788",
        "quic://[fd7a:115c:a1e0::1]",
        "quic://user@mini.example:8788",
        "quic://mini.example:8788/path",
        "quic://mini.example:8788?mode=fast",
        "quic://mini.example:0",
    ] {
        let error = classify_remote_endpoint(endpoint).expect_err(endpoint);
        assert!(
            error.contains("HOST:PORT") || error.contains("must start"),
            "{endpoint}: {error}"
        );
    }

    let remote = RemoteClient::new("https://mini.example:8788".into(), 80, 24, None, None);
    let error = remote
        .connect()
        .expect_err("invalid scheme must not start a runtime");
    assert!(
        matches!(error, WireError::Runtime { reason } if reason.contains("invalid remote endpoint"))
    );
}

#[test]
fn agent_declarations_lower_once_and_absence_clears_the_existing_badge() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let terminal_id = ResourceId::local(7);
    let value = Some(br#"{"name":"agent","state":"blocked"}"#.to_vec());
    let mut projected = Vec::new();
    remote.project_event(
        Event::AgentMetadata {
            terminal_id: terminal_id.clone(),
            value: value.clone(),
        },
        &mut projected,
    );
    // Raw extension delivery remains available to the C ABI, but cannot
    // reintroduce duplicate or stale badge handling in this encoder.
    remote.project_event(
        Event::Frame(Box::new(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal_id.clone()),
            key: RESOURCE_AGENT_KEY.into(),
            value,
            actor: None,
        })),
        &mut projected,
    );
    remote.project_event(
        Event::AgentMetadata {
            terminal_id,
            value: None,
        },
        &mut projected,
    );
    assert_eq!(projected.len(), 2);
    assert!(matches!(&projected[0], WireEvent::AgentStateChanged { name, .. } if name == "agent"));
    assert!(matches!(&projected[1], WireEvent::AgentStateChanged { name, .. } if name.is_empty()));
}

#[test]
fn a_publication_keeps_lifecycle_boundaries_in_order_and_take_events_omits_them() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let terminal_id = ResourceId::local(7);
    let events = vec![
        Event::Bell {
            terminal_id: terminal_id.clone(),
        },
        Event::ConnectionLost {
            message: Some("reset".into()),
        },
        Event::ConnectionOpened {
            connection_epoch: 2,
        },
        Event::TopologyChanged,
        Event::AgentAskedState {
            terminal_id,
            asked: false,
        },
    ];
    let published = remote.publication(Observation {
        connection_epoch: 2,
        events: events.clone(),
        events_dropped: true,
        status: phux_client_runtime::control::Status::Attached,
        last_error: Some("reset".into()),
        topology: None,
    });
    assert_eq!(
        published,
        WirePublication {
            connection_epoch: 2,
            events: vec![
                WireEvent::Bell {
                    terminal_id: "local:7".into(),
                },
                WireEvent::ConnectionLost {
                    message: Some("reset".into()),
                },
                WireEvent::ConnectionOpened {
                    connection_epoch: 2,
                },
                WireEvent::TopologyChanged,
                WireEvent::AgentAskedState {
                    terminal_id: "local:7".into(),
                    asked: false,
                },
            ],
            events_dropped: true,
            status: WireStatus::Attached,
            last_error: Some("reset".into()),
            topology: None,
        }
    );
    // The established drain keeps its vocabulary.
    let mut legacy = Vec::new();
    for event in events {
        remote.project_event(event, &mut legacy);
    }
    assert!(!legacy.iter().any(|event| matches!(
        event,
        WireEvent::ConnectionLost { .. } | WireEvent::ConnectionOpened { .. }
    )));
    assert_eq!(legacy.len(), 3);
}

#[test]
fn an_unconnected_client_publishes_an_empty_first_incarnation() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let published = remote.take_publication();
    assert_eq!(published.connection_epoch, 0);
    assert!(published.events.is_empty());
    assert!(!published.events_dropped);
    assert_eq!(published.status, WireStatus::Connecting);
    assert_eq!(published.topology, None);
}

#[test]
fn asked_levels_lower_after_their_announcement_in_order() {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let terminal_id = ResourceId::local(7);
    let mut projected = Vec::new();
    for event in [
        Event::AgentAskedState {
            terminal_id: terminal_id.clone(),
            asked: true,
        },
        Event::AgentAsked {
            terminal_id: terminal_id.clone(),
            question_id: "q1".into(),
            text: "Proceed?".into(),
            suggestions: Vec::new(),
            waiting_seconds: None,
        },
        Event::AgentAskedState {
            terminal_id,
            asked: false,
        },
    ] {
        remote.project_event(event, &mut projected);
    }
    assert_eq!(
        projected,
        vec![
            WireEvent::AgentAskedState {
                terminal_id: "local:7".into(),
                asked: true,
            },
            WireEvent::AgentAsked {
                terminal_id: "local:7".into(),
                question_id: "q1".into(),
                text: "Proceed?".into(),
                suggestions: Vec::new(),
                waiting_seconds: None,
            },
            WireEvent::AgentAskedState {
                terminal_id: "local:7".into(),
                asked: false,
            },
        ]
    );
}
