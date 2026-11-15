//! The relay topology the `relay_*` suites share: a production relay on
//! loopback and servers whose connectors hold a route on it.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use phux_config::ConnectorConfigEntry;
use phux_dial::quic::{DialedStream, InnerTls};
use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::FrameKind;
use phux_relay::{RelayConfig, cert_fingerprint};
use phux_server::ServerConfig;
use phux_server::workload::WorkloadPaths;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, encode_frame};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::oneshot;

/// Write `contents` owner-only.
pub(crate) fn write_private(path: &Path, contents: impl AsRef<[u8]>) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, contents).expect("write secret");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod secret");
}

/// A relay config under `dir` on `listen` (port 0 for any) that routes
/// `route` for `token`.
pub(crate) fn relay_config(
    dir: &Path,
    listen: SocketAddr,
    route: &str,
    token: &[u8],
) -> RelayConfig {
    std::fs::write(
        dir.join("relay-tokens"),
        format!("{} {route}\n", hex::encode(token)),
    )
    .expect("relay route");
    RelayConfig {
        max_conns: 16,
        ..phux_relay::config_in(listen, dir)
    }
}

pub(crate) const fn loopback() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

/// The relay certificate pin under `dir`.
pub(crate) fn relay_pin(dir: &Path) -> String {
    cert_fingerprint(&dir.join("relay-cert.pem")).expect("relay pin")
}

/// A server with its own workload CA, identity, and stores under `dir`,
/// whose connector holds the route for `token` on `relay`.
pub(crate) struct RelayedServer {
    /// The CA that issued the server's certificate, which consumers pin.
    pub(crate) authority: String,
    /// A consumer bearer the server admits.
    pub(crate) bearer: Vec<u8>,
    pub(crate) paths: WorkloadPaths,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
}

impl RelayedServer {
    pub(crate) fn start(
        dir: &Path,
        relay: SocketAddr,
        relay_pin: &str,
        token: &[u8],
        configure: impl FnOnce(&mut ServerConfig, &WorkloadPaths),
    ) -> Self {
        std::fs::create_dir_all(dir).expect("server dir");
        let paths = WorkloadPaths::with_overrides(
            Some(dir.join("workload-ca.pem")),
            Some(dir.join("workload-ca.key")),
            Some(dir.join("workload-keys")),
        );
        let (cert, key) = (dir.join("remote-cert.pem"), dir.join("remote-key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("server identity");
        let authority = phux_server::transport::tls::presented_authority(&cert)
            .expect("chain")
            .expect("issued by the CA");
        write_private(
            &dir.join("connector-token"),
            format!("{}\n", hex::encode(token)),
        );
        let bearer = phux_server_testkit::embedder::mint_bearer(&dir.join("consumer-tokens"));
        let connector = ConnectorConfigEntry {
            relay: relay.to_string(),
            token_file: Some(dir.join("connector-token")),
            cert_fingerprint: Some(relay_pin.to_owned()),
        };
        let mut cfg = ServerConfig {
            socket_path: dir.join("phux.sock"),
            pre_seeded_session: None,
            seed_with_pty: false,
            seed_command: None,
            env: phux_server::ServerEnv {
                ws_tokens: Some(dir.join("consumer-tokens")),
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..phux_server::ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        configure(&mut cfg, &paths);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            phux_server_testkit::terminal_server(cfg)
                .connectors(vec![connector], None)
                .run_async(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            authority,
            bearer,
            paths,
            stop,
            task,
        }
    }

    pub(crate) async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.expect("server task").expect("shutdown");
    }
}

/// An end-to-end session to the server behind `route`, pinning `ca`.
pub(crate) fn inner(ca: &str, identity: TlsClientIdentity, route: &str) -> InnerTls {
    InnerTls {
        trust: CertTrust::Authority { ca: ca.to_owned() },
        identity,
        server_name: route.to_owned(),
    }
}

/// A consumer dial of `route` through the relay at `addr`.
pub(crate) fn plan(
    addr: SocketAddr,
    relay_pin: &str,
    route: &str,
    token: Option<Vec<u8>>,
    inner: Option<InnerTls>,
) -> QuicDial {
    QuicDial {
        addr,
        server_name: route.to_owned(),
        token,
        trust: CertTrust::Pinned(relay_pin.to_owned()),
        identity: Some(TlsClientIdentity::None),
        inner,
    }
}

pub(crate) fn hello(client_name: &str) -> FrameKind {
    FrameKind::Hello {
        client_name: client_name.to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    }
}

pub(crate) async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    frame: &FrameKind,
) -> std::io::Result<()> {
    writer.write_all(&encode_frame(frame)).await?;
    writer.flush().await
}

/// One frame, which must be consumed whole: framing survived both hops.
pub(crate) async fn read_frame(
    reader: &mut (impl AsyncRead + Unpin),
) -> std::io::Result<FrameKind> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let mut framed = header.to_vec();
    framed.resize(4 + u32::from_be_bytes(header) as usize, 0);
    reader.read_exact(&mut framed[4..]).await?;
    let (frame, rest) = FrameKind::decode(&framed)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    assert!(rest.is_empty(), "framing survived both hops");
    Ok(frame)
}

/// HELLO through `plan`; the server's first answer, or why the dial failed.
pub(crate) async fn quic_hello(plan: &QuicDial, client_name: &str) -> Result<FrameKind, DialError> {
    let (_endpoint, conn, stream) = phux_dial::quic::dial_stream(plan).await?;
    let io = async {
        let (mut reader, mut writer): (
            Box<dyn tokio::io::AsyncRead + Unpin>,
            Box<dyn tokio::io::AsyncWrite + Unpin>,
        ) = match stream {
            DialedStream::Plain { send, recv } => (Box::new(recv), Box::new(send)),
            DialedStream::Inner(stream) => {
                let (recv, send) = tokio::io::split(*stream);
                (Box::new(recv), Box::new(send))
            }
        };
        write_frame(&mut writer, &hello(client_name)).await?;
        read_frame(&mut reader).await
    };
    tokio::time::timeout(WIRE_RECV_TIMEOUT, io)
        .await
        .map_err(|_| DialError::Connect("no answer".to_owned()))?
        .map_err(|err| {
            conn.close_reason().map_or_else(
                || DialError::Connect(err.to_string()),
                |close| phux_dial::quic::close_error(&close),
            )
        })
}
