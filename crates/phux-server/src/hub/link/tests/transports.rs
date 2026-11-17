//! The network and SSH transports against real listeners and stub programs.

use super::*;

#[tokio::test]
async fn net_link_preserves_satellite_extended_features_from_hello_ok() {
    let (port, server) = ws_satellite(|_| {
        let mut caps = phux_protocol::caps::ServerCapabilities::default();
        caps.features_ext =
            ServerFeatureExtSet::with(&[phux_protocol::caps::ServerFeatureExt::PathQuery]);
        hello_ok(
            caps,
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        )
    })
    .await;
    let conn = NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default())
        .connect(&SatelliteHost::new("sat"), &loopback_ws(port), None)
        .await
        .unwrap();
    let features = conn.server_features_ext().unwrap();
    assert!(features.contains(phux_protocol::caps::ServerFeatureExt::PathQuery));
    server.await.unwrap();
}

/// The link offers the hub's own Terminal profiles, so a native satellite
/// selects the same `NativeState` the hub selects for a native downstream
/// client, and the relay admits that client's satellite attach.
#[tokio::test]
async fn net_link_selects_the_profile_the_hub_selects_for_native_clients() {
    use phux_protocol::caps::{
        BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
        select_bootstrap_profile,
    };
    let native = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    );
    let (port, server) = ws_satellite(move |client_caps| {
        // Select exactly as a native satellite's HELLO handler does.
        let (profile, limits) = select_bootstrap_profile(&client_caps, &native).unwrap();
        hello_ok(
            phux_protocol::caps::ServerCapabilities::default(),
            profile,
            limits,
        )
    })
    .await;
    let conn = NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default())
        .with_bootstrap(native)
        .connect(&SatelliteHost::new("sat"), &loopback_ws(port), None)
        .await
        .unwrap();
    let downstream = ClientCapabilities::new().with_bootstrap(native);
    let (hub_selects, hub_limits) = select_bootstrap_profile(&downstream, &native).unwrap();
    assert!(matches!(hub_selects, BootstrapProfile::NativeState { .. }));
    assert_eq!(conn.bootstrap_profile().unwrap(), hub_selects);
    assert_eq!(conn.bootstrap_limits().unwrap(), hub_limits);
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

            let Supervised {
                statuses,
                cancel,
                host,
                relay: _relay,
                task: _task,
            } = supervise(
                entry(&format!("ws://127.0.0.1:{}", addr.port()), None, None),
                NetLinkTransport::new("ssh".into(), crate::hub::adopt::SatelliteAdopter::default()),
            );

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
    hello_ok(
        phux_protocol::caps::ServerCapabilities::default(),
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
}

fn hello_ok(
    server_caps: phux_protocol::caps::ServerCapabilities,
    profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Vec<u8> {
    let mut encoded = bytes::BytesMut::new();
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps,
        server_id: vec![0; 16],
        selected_profile: Some(profile),
        bootstrap_limits,
    }
    .encode(&mut encoded);
    encoded.to_vec()
}

/// A loopback WS satellite answering the first HELLO with `reply(caps)`.
async fn ws_satellite(
    reply: impl FnOnce(phux_protocol::caps::ClientCapabilities) -> Vec<u8> + Send + 'static,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let next = futures_util::StreamExt::next(&mut ws).await.unwrap();
        let tokio_tungstenite::tungstenite::Message::Binary(hello) = next.unwrap() else {
            panic!("expected binary HELLO");
        };
        let FrameKind::Hello { client_caps, .. } = FrameKind::decode(&hello).unwrap().0 else {
            panic!("expected HELLO");
        };
        let message = tokio_tungstenite::tungstenite::Message::Binary(reply(client_caps).into());
        futures_util::SinkExt::send(&mut ws, message).await.unwrap();
    });
    (port, server)
}

fn loopback_ws(port: u16) -> DialSpec {
    DialSpec::Ws {
        url: format!("ws://127.0.0.1:{port}"),
        trust: CertTrust::SkipVerify,
        token_file: None,
        identity: None,
    }
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
            let transport =
                NetLinkTransport::new(stub.into(), crate::hub::adopt::SatelliteAdopter::default());
            let Supervised {
                statuses,
                cancel,
                host,
                relay: _relay,
                task: _task,
            } = supervise(ssh_entry(), transport);

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
            let transport =
                NetLinkTransport::new(stub.into(), crate::hub::adopt::SatelliteAdopter::default());
            let Supervised {
                statuses,
                cancel,
                host,
                relay: _relay,
                task: _task,
            } = supervise(ssh_entry(), transport);

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
            let transport =
                NetLinkTransport::new(stub.into(), crate::hub::adopt::SatelliteAdopter::default());
            let Supervised {
                statuses,
                cancel,
                host,
                relay: _relay,
                task,
            } = supervise(ssh_entry(), transport);

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
            let transport = NetLinkTransport::new(
                "/nonexistent/phux-test-no-such-ssh".into(),
                crate::hub::adopt::SatelliteAdopter::default(),
            );
            let Supervised {
                statuses,
                cancel,
                host,
                relay: _relay,
                task: _task,
            } = supervise(ssh_entry(), transport);
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
