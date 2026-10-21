//! The network and SSH transports against real listeners and stub programs.

use super::*;

#[tokio::test]
async fn net_link_preserves_satellite_extended_features_from_hello_ok() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let _hello = futures_util::StreamExt::next(&mut ws)
            .await
            .unwrap()
            .unwrap();
        let mut caps = phux_protocol::caps::ServerCapabilities::default();
        caps.features_ext =
            ServerFeatureExtSet::with(&[phux_protocol::caps::ServerFeatureExt::PathQuery]);
        let mut encoded = bytes::BytesMut::new();
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: caps,
            server_id: vec![0; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        }
        .encode(&mut encoded);
        futures_util::SinkExt::send(
            &mut ws,
            tokio_tungstenite::tungstenite::Message::Binary(encoded.to_vec().into()),
        )
        .await
        .unwrap();
    });
    let spec = DialSpec::Ws {
        url: format!("ws://127.0.0.1:{port}"),
        trust: CertTrust::SkipVerify,
        token_file: None,
    };
    let conn = NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default())
        .connect(&SatelliteHost::new("sat"), &spec, None)
        .await
        .unwrap();
    assert!(
        conn.server_features_ext()
            .unwrap()
            .contains(phux_protocol::caps::ServerFeatureExt::PathQuery)
    );
    server.await.unwrap();
}

#[tokio::test]
async fn net_transport_connects_to_a_loopback_ws_listener_and_notices_the_drop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let addr = listener.local_addr().expect("addr");
            let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::task::spawn_local(async move {
                let (tcp, _) = listener.accept().await.expect("accept");
                let mut ws = tokio_tungstenite::accept_async(tcp)
                    .await
                    .expect("ws handshake");
                let hello = futures_util::StreamExt::next(&mut ws)
                    .await
                    .expect("HELLO message")
                    .expect("HELLO read");
                let tokio_tungstenite::tungstenite::Message::Binary(hello) = hello else {
                    panic!("expected binary HELLO");
                };
                assert!(matches!(
                    FrameKind::decode(&hello).expect("decode HELLO").0,
                    FrameKind::Hello { .. }
                ));
                let hello_ok = synthesized_hello_ok();
                futures_util::SinkExt::send(
                    &mut ws,
                    tokio_tungstenite::tungstenite::Message::Binary(hello_ok.into()),
                )
                .await
                .expect("send HELLO_OK");
                let _ = drop_rx.await;
                drop(ws);
                drop(listener);
            });

            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            tokio::task::spawn_local(run_link(
                host.clone(),
                entry(&format!("ws://127.0.0.1:{}", addr.port()), None, None),
                NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default()),
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));

            wait_for_real_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

            drop_tx.send(()).expect("server alive");
            wait_for_real_status(&statuses, &host, |s| {
                matches!(s, LinkStatus::Backoff { .. })
            })
            .await;

            cancel.cancel();
            server.await.expect("server task");
        })
        .await;
}

pub(super) fn synthesized_hello_ok() -> Vec<u8> {
    let mut encoded = bytes::BytesMut::new();
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: phux_protocol::caps::ServerCapabilities::default(),
        server_id: vec![0; 16],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
    .encode(&mut encoded);
    encoded.to_vec()
}

pub(super) fn shell_octal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut escaped = String::with_capacity(bytes.len() * 5);
    for byte in bytes {
        write!(escaped, "\\0{byte:03o}").expect("write to String");
    }
    escaped
}

/// Write an executable stub script and return its path.
pub(super) fn write_stub(dir: &std::path::Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-ssh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    path
}

pub(super) fn ssh_entry() -> HubEntry {
    entry("ssh://me@devbox:2222", None, None)
}

#[tokio::test]
async fn ssh_transport_spawns_the_planned_argv_and_treats_exit_as_a_drop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let argv_file = dir.path().join("argv.txt");
            let stub = write_stub(
                dir.path(),
                &format!(
                    "printf '%s\\n' \"$@\" > {}\necho 'stub bridge refused' >&2\nexit 7",
                    argv_file.display()
                ),
            );
            let transport = NetLinkTransport {
                ssh_program: stub.into(),
                adopter: crate::hub::adopt::SatelliteAdopter::default(),
            };
            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            tokio::task::spawn_local(run_link(
                host.clone(),
                ssh_entry(),
                transport,
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));

            wait_for_real_status(&statuses, &host, |s| {
                matches!(
                    s,
                    LinkStatus::Backoff { last_error, .. }
                        if last_error.contains("stub bridge refused")
                            && last_error.contains('7')
                )
            })
            .await;
            cancel.cancel();

            // The stub saw exactly the planner's argv, host after `--`.
            let recorded = std::fs::read_to_string(&argv_file).expect("argv recorded");
            let expected: Vec<String> = ssh_argv(Some("me"), "devbox", Some(2222));
            assert_eq!(
                recorded.lines().collect::<Vec<_>>(),
                expected.iter().map(String::as_str).collect::<Vec<_>>()
            );
        })
        .await;
}

#[tokio::test]
async fn ssh_transport_drains_stderr_past_the_pipe_buffer_without_stalling() {
    // A remote writing more than a pipe buffer to fd 2 must not wedge
    // the link, and the tail keeps the end of the stream.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // ~200 KiB of stderr, a marker, and a nonzero exit.
            let stub = write_stub(
                dir.path(),
                "i=0\n\
                     while [ \"$i\" -lt 200 ]; do printf '%01024d' 0 >&2; i=$((i + 1)); done\n\
                     echo 'DRAINED-TO-EOF' >&2\n\
                     exit 3",
            );
            let transport = NetLinkTransport {
                ssh_program: stub.into(),
                adopter: crate::hub::adopt::SatelliteAdopter::default(),
            };
            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            tokio::task::spawn_local(run_link(
                host.clone(),
                ssh_entry(),
                transport,
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));

            wait_for_real_status(&statuses, &host, |s| {
                matches!(
                    s,
                    LinkStatus::Backoff { last_error, .. }
                        if last_error.contains("DRAINED-TO-EOF")
                            && last_error.contains('3')
                )
            })
            .await;
            cancel.cancel();
        })
        .await;
}

#[tokio::test]
async fn ssh_transport_holds_a_live_child_as_connected() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // Stays up reading stdin until the pipe closes or it is killed.
            let reply = shell_octal(&synthesized_hello_ok());
            let stub = write_stub(
                dir.path(),
                &format!("printf '%b' '{reply}'\nexec cat > /dev/null"),
            );
            let transport = NetLinkTransport {
                ssh_program: stub.into(),
                adopter: crate::hub::adopt::SatelliteAdopter::default(),
            };
            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            let task = tokio::task::spawn_local(run_link(
                host.clone(),
                ssh_entry(),
                transport,
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));

            wait_for_real_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("supervisor exits on cancel")
                .expect("no panic");
        })
        .await;
}

#[tokio::test]
async fn ssh_transport_missing_program_is_a_failed_attempt_not_a_panic() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let transport = NetLinkTransport {
                ssh_program: "/nonexistent/phux-test-no-such-ssh".into(),
                adopter: crate::hub::adopt::SatelliteAdopter::default(),
            };
            let statuses = HubLinkStatuses::default();
            let cancel = CancellationToken::new();
            let host = host();
            let (_relay, relay_rx) = relay_pair(&host);
            tokio::task::spawn_local(run_link(
                host.clone(),
                ssh_entry(),
                transport,
                statuses.clone(),
                relay_rx,
                cancel.child_token(),
            ));
            wait_for_real_status(&statuses, &host, |s| {
                matches!(
                    s,
                    LinkStatus::Backoff { last_error, .. } if last_error.contains("spawn")
                )
            })
            .await;
            cancel.cancel();
        })
        .await;
}

/// Real-time sibling of [`wait_for_status`] for the loopback test.
pub(super) async fn wait_for_real_status(
    statuses: &HubLinkStatuses,
    host: &SatelliteHost,
    want: impl Fn(&LinkStatus) -> bool,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if statuses.get(host).as_ref().is_some_and(&want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("status never matched; last = {:?}", statuses.get(host));
}
