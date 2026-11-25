//! Attach lookup, ending and transport-shape contracts.

use std::time::{Duration, Instant};

use phux_client_runtime::reconnect::Ladder;
use phux_dial::{CertTrust, QuicDial, WsDial};
use phux_protocol::wire::frame::DetachReason;

use super::native::{OVERLAY_REACHABILITY_HINT, reachability_hint};
use super::reconnect::{ReconnectOutcome, ReconnectPolicy, UDS_RECONNECT, reconnect_failure_lines, reconnect_policy, reconnect_progress_line, wait_until_connectable};
use super::remote::remote_repair;
use super::*;

mod reconnect;
mod ws_hello;

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
    /// The UDS policy with a test-length deadline; cadence untouched.
    fn with_deadline(deadline: Duration) -> ReconnectPolicy {
        ReconnectPolicy {
            deadline,
            ..UDS_RECONNECT
        }
    }
