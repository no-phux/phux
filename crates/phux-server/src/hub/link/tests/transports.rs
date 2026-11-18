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
    let conn = net_transport("ssh")
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
    let conn = net_transport("ssh")
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

/// The real network/SSH transport, dialing SSH through `ssh_program`.
pub(super) fn net_transport(ssh_program: impl Into<std::ffi::OsString>) -> NetLinkTransport {
    NetLinkTransport::new(
        ssh_program.into(),
        crate::hub::adopt::SatelliteAdopter::default(),
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
    on_local(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let argv_file = dir.path().join("argv.txt");
        let stub = write_stub(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > {}\necho 'stub bridge refused' >&2\nexit 7",
                argv_file.display()
            ),
        );
        let link = supervise(ssh_entry(), net_transport(stub));

        link.backoff_real(&["stub bridge refused", "7"]).await;
        link.cancel.cancel();

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
    on_local(async {
        let dir = tempfile::tempdir().expect("tempdir");
        // ~200 KiB of stderr, a marker, and a nonzero exit.
        let stub = write_stub(
            dir.path(),
            "i=0\n\
                 while [ \"$i\" -lt 200 ]; do printf '%01024d' 0 >&2; i=$((i + 1)); done\n\
                 echo 'DRAINED-TO-EOF' >&2\n\
                 exit 3",
        );
        let link = supervise(ssh_entry(), net_transport(stub));

        link.backoff_real(&["DRAINED-TO-EOF", "3"]).await;
        link.cancel.cancel();
    })
    .await;
}

#[tokio::test]
async fn ssh_transport_missing_program_is_a_failed_attempt_not_a_panic() {
    on_local(async {
        let link = supervise(
            ssh_entry(),
            net_transport("/nonexistent/phux-test-no-such-ssh"),
        );
        link.backoff_real(&["spawn"]).await;
        link.cancel.cancel();
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
