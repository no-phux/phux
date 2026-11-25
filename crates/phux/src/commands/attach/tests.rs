//! Attach contracts and dial planning regressions.
use super::*;
use std::time::Duration;
use phux_client::attach::{CertTrust, QuicDial, WsDial};
mod reconnect;
use crate::commands::attach::reconnect::{ReconnectOutcome, wait_until_connectable};
use self::reconnect::with_deadline;
    /// The overlay hint fires only for a reachability failure on a
    /// non-loopback target — never for pin/auth failures (a host that
    /// answered) and never for loopback (no overlay involved).
    #[test]
    fn reachability_hint_gates_on_variant_and_loopback() {
        let unreachable = AttachError::Unreachable("x".to_owned());
        assert_eq!(
            reachability_hint(&unreachable, false),
            Some(OVERLAY_REACHABILITY_HINT)
        );
        assert_eq!(reachability_hint(&unreachable, true), None);

        let pin_mismatch = AttachError::Connect(
            "server certificate fingerprint mismatch (pinned AA, got BB)".to_owned(),
        );
        assert_eq!(reachability_hint(&pin_mismatch, false), None);

        let io = AttachError::Io(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(reachability_hint(&io, false), None);
    }

    #[test]
    fn remote_repair_is_limited_to_early_transport_failures() {
        // A refused credential means a host answered: only a re-pair helps.
        // Nobody answering means the server is most likely stopped.
        for (result, repair) in [
            (
                Err(AttachError::Connect("bad credential".to_owned())),
                Repair::RePair,
            ),
            (
                Err(AttachError::Unreachable("no route".to_owned())),
                Repair::Start,
            ),
            (
                Err(AttachError::Refused("no such session".to_owned())),
                Repair::None,
            ),
            (Err(AttachError::Disconnected), Repair::None),
            (Ok(AttachEnd::Detached { reason: None }), Repair::None),
        ] {
            assert_eq!(remote_repair(&result), repair);
        }
    }

    /// `--quic` targets split on the last `:`, so IPv4 literals, bracketed
    /// IPv6 literals, and DNS names all parse; a missing or malformed port
    /// is rejected up front with a usage error.
    #[test]
    fn split_host_port_accepts_documented_target_shapes() {
        assert_eq!(
            split_host_port("127.0.0.1:8788"),
            Ok(("127.0.0.1", 8788_u16))
        );
        assert_eq!(split_host_port("[::1]:1"), Ok(("[::1]", 1_u16)));
        assert_eq!(
            split_host_port("myhost.tailnet-name.ts.net:8788"),
            Ok(("myhost.tailnet-name.ts.net", 8788_u16))
        );

        for (target, reason) in [
            ("myhost.tailnet-name.ts.net", "missing a port"),
            ("myhost:notaport", "invalid port"),
            (":8788", "missing a host"),
        ] {
            let err = split_host_port(target).expect_err(target);
            assert!(err.contains(reason), "{target}: {err}");
        }
    }

    /// A relay route with no CA pinned is refused by the name the caller
    /// gave (the registry entry), not the dialed address, with the CLI's
    /// own remedy appended to the runtime's transport-neutral reason.
    #[test]
    fn an_unpinned_relay_route_is_refused_by_name_with_the_cli_remedy() {
        let plan = DialPlan {
            dial: Dial::Quic(phux_client::attach::QuicDial {
                addr: "192.0.2.1:4433".parse().expect("addr"),
                server_name: "studio-route".into(),
                token: None,
                trust: CertTrust::Pinned("relay-leaf".into()),
                identity: Some(phux_dial::TlsClientIdentity::None),
                inner: None,
            }),
            loopback: false,
        };
        let pin = phux_client_runtime::target::AuthorityPin {
            ca: None,
            route: Some("studio-route".into()),
        };
        let Err(DialRefusal::Malformed(refused)) = plan.with_authority("studio", &pin) else {
            panic!("a relay route without the server's CA must be refused");
        };
        assert!(refused.starts_with("studio is reached"), "{refused}");
        assert!(!refused.contains("192.0.2.1"), "{refused}");
        assert!(refused.contains("--relay-route studio-route"), "{refused}");
        assert!(refused.contains("--ca-fingerprint"), "{refused}");
    }
    /// Only a pane attaching to its own server is refused, and the override
    /// or a different socket lets it through.
    #[test]
    fn nested_attach_is_refused_only_against_the_panes_own_server() {
        let own = Path::new("/tmp/phux-nested-test/phux.sock");
        let other = Path::new("/tmp/phux-nested-test/other.sock");
        assert!(is_nested_attach(own, true, Some(own), false));
        assert!(!is_nested_attach(own, true, Some(own), true), "override");
        assert!(
            !is_nested_attach(other, true, Some(own), false),
            "other server"
        );
        assert!(
            !is_nested_attach(own, false, Some(own), false),
            "not in a pane"
        );
        assert!(!is_nested_attach(own, true, None, false), "no pane socket");
    }

    /// The typed refusal renders the exact lines attach printed before the
    /// planners stopped printing: the flag-naming remedy for attach, and the
    /// overlay hint only after a DNS name failed to resolve.
    #[test]
    fn dial_refusals_keep_attach_wording() {
        let unpinned = DialRefusal::UnpinnedQuic {
            target: "mini:8788".to_owned(),
        }
        .attach_lines();
        assert_eq!(
            unpinned,
            vec![
                "refusing to dial non-loopback QUIC server mini:8788 without --cert-fingerprint.",
                "Run `phux pair` on the server host to print its certificate fingerprint,",
                "then pass it: phux attach --quic mini:8788 --cert-fingerprint <FP>",
            ]
        );

        let unresolved = |dns_name| {
            DialRefusal::Unresolved {
                target: "mini:8788".to_owned(),
                detail: "name resolution failed: nope".to_owned(),
                dns_name,
            }
            .attach_lines()
        };
        assert_eq!(
            unresolved(true),
            vec![
                "QUIC attach to mini:8788 failed: name resolution failed: nope".to_owned(),
                OVERLAY_REACHABILITY_HINT.to_owned(),
            ]
        );
        assert_eq!(
            unresolved(false).len(),
            1,
            "an IP literal never earns the hint"
        );

        assert_eq!(
            DialRefusal::NoToken {
                url: "wss://h:1".to_owned()
            }
            .attach_lines()[0],
            "refusing remote WebSocket attach to wss://h:1 without --token."
        );
    }

    /// The gate is whether the dial actually crosses a network, not whether
    /// the transport could: a loopback QUIC or WebSocket dial has no round
    /// trip to hide, so it must not predict.
    #[test]
    fn only_a_dial_that_crosses_a_network_leaves_the_machine() {
        let uds = Dial::uds(std::path::Path::new("/tmp/phux-test.sock"));
        let quic_loopback = quic_dial("127.0.0.1:8788");
        let quic_remote = quic_dial("203.0.113.7:8788");
        let ws_loopback = ws_dial("ws://127.0.0.1:8787");
        let ws_localhost = ws_dial("ws://localhost:8787");
        let ws_remote = ws_dial("wss://example.invalid:8787");

        // Loopback QUIC/ws (and `localhost` by name) is a network transport
        // with no network between the ends: no latency to hide, only mispaints.
        for local in [&uds, &quic_loopback, &ws_loopback, &ws_localhost] {
            assert!(!dial_leaves_the_machine(local), "{local:?}");
        }
        assert!(dial_leaves_the_machine(&quic_remote));
        assert!(dial_leaves_the_machine(&ws_remote));
    }

    fn quic_dial(addr: &str) -> Dial {
        Dial::Quic(QuicDial {
            addr: addr.parse().expect("addr"),
            server_name: "localhost".to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            identity: None,
            inner: None,
        })
    }

    fn ws_dial(url: &str) -> Dial {
        Dial::Ws(WsDial {
            url: url.to_owned(),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
            identity: None,
        })
    }

    /// `phux attach X` against a live server that has no `X` used to print
    /// only the server's refusal. Attach stays lookup-only (tmux's `attach -t`
    /// refuses the same way), so the miss names the verb that creates it.
    #[test]
    fn a_missing_session_names_the_create_and_list_verbs() {
        let session = |name: &str| {
            phux_protocol::wire::info::SessionInfo::new(phux_protocol::ids::SessionId::new(1), name)
        };
        let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
            phux_protocol::ids::SessionId::new(1),
            phux_protocol::ids::WindowId::new(1),
            phux_protocol::ids::ResourceId::local(1),
        )
        .with_sessions(vec![session("main")]);
        assert!(session_is_missing(&snapshot, "work"));
        assert!(!session_is_missing(&snapshot, "main"));

        assert_eq!(
            missing_session_lines("work"),
            vec![
                "no session named \"work\" on this server".to_owned(),
                "create it with `phux new work`, or run `phux ls` to list sessions".to_owned(),
            ]
        );
        assert!(missing_session_lines("my work")[1].contains("`phux new 'my work'`"));
    }

    /// A detach the user chose says nothing; every other ending names its
    /// cause (phux-l83x), and a last-pane death names the exit shape.
    #[test]
    fn attach_end_explanation_covers_all_shapes() {
        let detached = |reason| AttachEnd::Detached { reason };
        let closed = |exit_status| AttachEnd::LastPaneClosed { exit_status };
        for (end, expected) in [
            (detached(None), None),
            (detached(Some(DetachReason::Requested)), None),
            (
                detached(Some(DetachReason::ServerShutdown)),
                Some("phux: detached: the server is shutting down"),
            ),
            (
                detached(Some(DetachReason::Replaced)),
                Some("phux: detached: another client took over this attach"),
            ),
            (
                closed(Some(0)),
                Some("phux: session ended: the last pane exited 0"),
            ),
            (
                closed(Some(137)),
                Some("phux: session ended: the last pane exited 137"),
            ),
            (
                closed(None),
                Some("phux: session ended: the last pane was killed"),
            ),
        ] {
            assert_eq!(end.explanation().as_deref(), expected);
        }
    }
use phux_protocol::wire::frame::DetachReason;
mod ws_hello;
