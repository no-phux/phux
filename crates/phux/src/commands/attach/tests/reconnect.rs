//! Reconnect deadlines, refusals, repair classification and diagnostics.

use super::*;

    /// The reconnect probe is three-way: missing socket is `SocketGone`, a
    /// bound listener is `Connectable`, a path that never accepts is `TimedOut`.
    #[tokio::test]
    async fn reconnect_probe_distinguishes_gone_live_and_dead_sockets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("probe.sock");

        // No socket file: nothing to reconnect to — returns without waiting.
        let start = Instant::now();
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_secs(5))).await,
            ReconnectOutcome::SocketGone
        ));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "a missing socket should fail fast, not burn the deadline"
        );

        // A bound listener: connectable.
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_secs(2))).await,
            ReconnectOutcome::Connectable(None)
        ));
        drop(listener);

        // A path that exists but never accepts: the deadline elapses.
        std::fs::remove_file(&path).ok();
        std::fs::File::create(&path).expect("plug the socket path");
        assert!(matches!(
            wait_until_connectable(&Dial::uds(&path), with_deadline(Duration::from_millis(300)))
                .await,
            ReconnectOutcome::TimedOut
        ));
    }

    #[tokio::test]
    async fn remote_peer_that_accepts_transport_but_never_negotiates_cannot_overrun_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind silent remote");
        let addr = listener.local_addr().expect("listener address");
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept reconnect probe");
            let _held_transport_open = stream;
            std::future::pending::<()>().await;
        });
        let dial = ws_dial_to(addr);
        let policy = ReconnectPolicy {
            deadline: Duration::from_millis(300),
            ladder: Ladder::flat(Duration::from_millis(50)),
        };
        let start = Instant::now();
        assert!(matches!(
            wait_until_connectable(&dial, policy).await,
            ReconnectOutcome::TimedOut
        ));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= policy.deadline,
            "deadline fired early: {elapsed:?}"
        );
        assert!(
            elapsed < policy.deadline + Duration::from_secs(2),
            "transport/protocol setup overran the absolute deadline: {elapsed:?}"
        );
        peer.abort();
        let _ = peer.await;
    }
    /// A TCP peer that answers every WebSocket upgrade with `status`,
    /// counting how many it was asked for. Enough HTTP for tungstenite to
    /// read a rejection: the client never gets past the handshake, so the
    /// body and the connection lifetime after it do not matter.
    fn refusing_ws_peer(
        listener: tokio::net::TcpListener,
        status: &'static str,
    ) -> (
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let peer = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte).await {
                        Ok(1) => request.push(byte[0]),
                        _ => break,
                    }
                }
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = stream.flush().await;
            }
        });
        (attempts, peer)
    }

    fn ws_dial_to(addr: std::net::SocketAddr) -> Dial {
        ws_dial(&format!("ws://{addr}"))
    }

    /// ADR-0133: a refused reconnect ends on the first probe with the refusal.
    /// The deadline is far longer than the test would survive if the ladder were
    /// walked.
    #[tokio::test]
    async fn a_refused_reconnect_ends_on_the_first_probe_with_the_real_reason() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind refusing remote");
        let addr = listener.local_addr().expect("listener address");
        let (attempts, peer) = refusing_ws_peer(listener, "401 Unauthorized");
        let dial = ws_dial_to(addr);

        let start = Instant::now();
        let ReconnectOutcome::Refused(err) = wait_until_connectable(
            &dial,
            ReconnectPolicy {
                deadline: Duration::from_secs(60),
                ladder: Ladder::INTERACTIVE,
            },
        )
        .await
        else {
            panic!("a 401 must end the reconnect, not walk the ladder");
        };
        assert!(
            err.to_string().contains("401"),
            "the refusal must carry the status the server sent: {err}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "a refusal must not wait out the deadline: {:?}",
            start.elapsed()
        );
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a refused dial must not be retried"
        );

        let lines =
            reconnect_failure_lines(&ReconnectOutcome::Refused(err), Duration::from_secs(60));
        assert!(
            lines[0].contains("refused the reconnect") && lines[0].contains("401"),
            "the report names the refusal, not a timeout: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("phux host add")),
            "a refused reconnect points at re-pairing: {lines:?}"
        );

        peer.abort();
        let _ = peer.await;
    }

    /// The other half of the same rule: a status that may heal is still
    /// worth the ladder, so the wait keeps probing until the deadline
    /// rather than treating any HTTP failure as terminal.
    #[tokio::test]
    async fn a_transient_refusal_still_walks_the_ladder() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky remote");
        let addr = listener.local_addr().expect("listener address");
        let (attempts, peer) = refusing_ws_peer(listener, "503 Service Unavailable");
        let dial = ws_dial_to(addr);

        assert!(
            matches!(
                wait_until_connectable(
                    &dial,
                    ReconnectPolicy {
                        deadline: Duration::from_millis(400),
                        ladder: Ladder::flat(Duration::from_millis(50)),
                    },
                )
                .await,
                ReconnectOutcome::TimedOut
            ),
            "a 503 is not a refusal: the window must close on the deadline"
        );
        assert!(
            attempts.load(std::sync::atomic::Ordering::SeqCst) > 1,
            "a transient failure must be retried, not given up on"
        );

        peer.abort();
        let _ = peer.await;
    }


    /// The local lane's numbers are pinned: 100ms flat over 10s keeps the
    /// graceful-upgrade blink invisible.
    #[test]
    fn uds_reconnect_policy_is_unchanged_flat_100ms_over_10s() {
        let policy = reconnect_policy(&Dial::uds(std::path::Path::new("/tmp/phux-test.sock")));

        assert_eq!(policy.deadline, Duration::from_secs(10));
        assert_eq!(policy.ladder.floor, Duration::from_millis(100));
        assert_eq!(
            policy.ladder.ceiling,
            Duration::from_millis(100),
            "UDS must not back off — a graceful upgrade is over in <1s"
        );
    }

    /// Both remote lanes get the patient, backing-off policy: each probe is a
    /// real handshake over a radio, and the thing that broke is usually the
    /// client's own network rather than the server.
    #[test]
    fn remote_lanes_get_backoff_and_a_longer_deadline() {
        let ws = reconnect_policy(&ws_dial("wss://example.ts.net:8788"));
        let quic = reconnect_policy(&quic_dial("127.0.0.1:8788"));

        assert_eq!(ws, quic, "the two remote lanes share one policy");
        assert!(
            ws.deadline > UDS_RECONNECT.deadline,
            "a wifi transition outlasts a re-exec: {:?}",
            ws.deadline
        );
        assert!(
            ws.ladder.ceiling > ws.ladder.floor,
            "remote probes must back off, not hammer TLS"
        );
        assert_eq!(
            ws.ladder,
            Ladder::INTERACTIVE,
            "the remote lanes walk the runtime's interactive ladder (ADR-0133)"
        );
    }

    /// phux-i0e8.2.3: the countdown line is `\r`-overwritten in place, so
    /// the format is pinned exactly; remaining time rounds UP to whole
    /// seconds so it opens at the full deadline and never reads `0s`
    /// mid-wait.
    #[test]
    fn reconnect_progress_line_rounds_up_and_pins_format() {
        for (left, seconds) in [(10_000, 10), (9_100, 10), (200, 1), (0, 0)] {
            assert_eq!(
                reconnect_progress_line(Duration::from_millis(left)),
                format!("phux: reconnecting… {seconds}s left (Ctrl-C to give up)")
            );
        }
    }

    /// phux-i0e8.2.3: the two reconnect-window failure shapes are distinct
    /// sentences — a gone socket is a clean shutdown, a timeout is a crash
    /// — and BOTH name `phux doctor` as the remedy.
    #[test]
    fn reconnect_failure_lines_are_distinct_and_name_doctor() {
        let deadline = Duration::from_secs(10);

        let gone = reconnect_failure_lines(&ReconnectOutcome::SocketGone, deadline);
        assert_eq!(
            gone[0],
            "the server shut down (its socket is gone) and is not restarting"
        );
        assert!(
            gone.iter().any(|l| l.contains("phux doctor")),
            "SocketGone must name phux doctor: {gone:?}"
        );

        let timed_out = reconnect_failure_lines(&ReconnectOutcome::TimedOut, deadline);
        assert_eq!(
            timed_out[0],
            "the server did not come back within 10s — it may have crashed"
        );
        assert!(
            timed_out.iter().any(|l| l.contains("phux doctor")),
            "TimedOut must name phux doctor: {timed_out:?}"
        );
        assert!(
            timed_out.iter().any(|l| l.starts_with("server log: ")),
            "TimedOut points at the server log (the crash reason lives there): {timed_out:?}"
        );

        assert_ne!(gone[0], timed_out[0], "the two failures read differently");

        let host_key = reconnect_failure_lines(
            &ReconnectOutcome::HostKey("remove the old key with `ssh-keygen -R box`".to_owned()),
            deadline,
        );
        assert!(
            host_key[0].contains("host's key") && host_key[1].contains("ssh-keygen -R box"),
            "a refused host key names the fix, not a crash: {host_key:?}"
        );
        assert!(
            reconnect_failure_lines(&ReconnectOutcome::Connectable(None), deadline).is_empty(),
            "a successful reconnect has nothing to report"
        );
    }

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
