//! Production relay ↔ dial-out connector integration (phux-81yr/phux-pt5m).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_config::ConnectorConfigEntry;
use phux_dial::{CertTrust, QuicDial};
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{RelayConfig, cert_fingerprint};
use phux_server::{ServerConfig, ServerRuntime};
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use phux_server_testkit::relay::RelayHarness;
const ROUTE: &str = "connector-test";
const PING_NONCE: u64 = 0xC011_EC70;
const TUNNEL_TOKEN: [u8; 32] = [0x11; 32];
const CONSUMER_TOKEN: [u8; 32] = [0x22; 32];

/// Ceiling for reading one framed reply off the connector's QUIC stream.
///
/// Not load-bearing: every assertion in this file is on the DECODED frame,
/// never on how quickly it arrived. The 2s it replaces was generous on an
/// idle laptop and a measurement of the scheduler on a saturated one
/// (phux-br1f). A stream that never delivers still fails, just later, and the
/// error string still names which half (header vs body) timed out.
const FRAME_READ_DEADLINE: Duration = Duration::from_secs(30);

fn encode(frame: &FrameKind) -> BytesMut {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    bytes
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<FrameKind, String> {
    let mut header = [0_u8; 4];
    timeout(FRAME_READ_DEADLINE, recv.read_exact(&mut header))
        .await
        .map_err(|_| "frame header timed out".to_owned())?
        .map_err(|err| err.to_string())?;
    let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
    timeout(FRAME_READ_DEADLINE, recv.read_exact(&mut body))
        .await
        .map_err(|_| "frame body timed out".to_owned())?
        .map_err(|err| err.to_string())?;
    let mut framed = header.to_vec();
    framed.extend_from_slice(&body);
    let (frame, rest) = FrameKind::decode(&framed).map_err(|err| err.to_string())?;
    if !rest.is_empty() {
        return Err("decoder left trailing bytes".to_owned());
    }
    Ok(frame)
}

async fn ping(
    addr: SocketAddr,
    fingerprint: &str,
    route: &str,
    token: Option<Vec<u8>>,
) -> Result<FrameKind, String> {
    let dial = QuicDial {
        addr,
        server_name: route.to_owned(),
        token,
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    };
    let (_endpoint, _connection, mut send, mut recv) = phux_dial::quic::dial(&dial)
        .await
        .map_err(|err| err.to_string())?;
    send.write_all(&encode(&FrameKind::Ping { nonce: PING_NONCE }))
        .await
        .map_err(|err| err.to_string())?;
    read_frame(&mut recv).await
}

async fn wait_for_pong(addr: SocketAddr, fingerprint: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match ping(addr, fingerprint, ROUTE, Some(CONSUMER_TOKEN.to_vec())).await {
            Ok(FrameKind::Pong { nonce }) if nonce == PING_NONCE => return,
            Ok(frame) => last_error = format!("unexpected frame: {frame:?}"),
            Err(err) => last_error = err,
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("connector never served a consumer PING: {last_error}");
}

#[cfg(unix)]
fn write_token(path: &Path, token: &[u8]) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::write(path, format!("{}\n", hex::encode(token))).expect("write token");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("set token owner-only");
}

fn relay_config(dir: &Path, listen: SocketAddr) -> RelayConfig {
    RelayConfig {
        listen,
        cert_path: dir.join("relay-cert.pem"),
        key_path: dir.join("relay-key.pem"),
        tokens_path: dir.join("relay-tokens"),
        max_conns: 16,
    }
}

#[test]
fn connector_bridges_consumers_rejects_bad_auth_and_redials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let relay_tokens = dir.path().join("relay-tokens");
    std::fs::write(
        &relay_tokens,
        format!("{} {ROUTE}\n", hex::encode(TUNNEL_TOKEN)),
    )
    .expect("write relay route");
    let connector_token = dir.path().join("connector-token");
    write_token(&connector_token, &TUNNEL_TOKEN);
    let consumer_tokens = dir.path().join("consumer-tokens");
    write_token(&consumer_tokens, &CONSUMER_TOKEN);

    // This integration binary owns the process and runs one test, so the
    // process-wide override cannot race another test.
    unsafe { std::env::set_var("PHUX_WS_TOKENS", &consumer_tokens) };

    let socket_path: PathBuf = dir.path().join("phux.sock");
    phux_server_testkit::run_local(async {
        let initial_config = relay_config(
            dir.path(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        );
        let relay = RelayHarness::start(initial_config);
        let relay_addr = relay.addr;
        let fingerprint = cert_fingerprint(&dir.path().join("relay-cert.pem"))
            .expect("relay certificate fingerprint");

        let connector = ConnectorConfigEntry {
            relay: relay_addr.to_string(),
            token_file: Some(connector_token.clone()),
            cert_fingerprint: Some(fingerprint.clone()),
        };
        let cfg = ServerConfig {
            socket_path,
            pre_seeded_session: None,
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        let (stop_server, server_stopped) = oneshot::channel();
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(cfg)
                .connectors(vec![connector], None)
                .run_async(async move {
                    let _ = server_stopped.await;
                })
                .await
        });

        // The production relay creates a fresh reverse bidi stream; the
        // connector feeds it into the ordinary server consumer dispatch.
        wait_for_pong(relay_addr, &fingerprint).await;

        // A consumer bearer is still checked by the server behind the relay.
        let bad_auth = ping(relay_addr, &fingerprint, ROUTE, Some(vec![0x33; 32])).await;
        assert!(
            bad_auth.is_err(),
            "bad consumer token unexpectedly reached dispatch"
        );
        wait_for_pong(relay_addr, &fingerprint).await;

        // Route identity is TLS SNI. An unknown name fails before wire bytes.
        let wrong_route = ping(
            relay_addr,
            &fingerprint,
            "unknown-route",
            Some(CONSUMER_TOKEN.to_vec()),
        )
        .await;
        assert!(wrong_route.is_err(), "unknown route unexpectedly connected");

        // Kill and recreate the relay on the same address. The connector's
        // supervisor must notice the lost tunnel and redial without a restart.
        relay.stop().await;
        let restarted_config = relay_config(dir.path(), relay_addr);
        let restarted = RelayHarness::start(restarted_config);
        assert_eq!(restarted.addr, relay_addr);
        wait_for_pong(relay_addr, &fingerprint).await;

        let _ = stop_server.send(());
        server
            .await
            .expect("server task panicked")
            .expect("server shutdown");
        restarted.stop().await;
    });

    unsafe { std::env::remove_var("PHUX_WS_TOKENS") };
}
