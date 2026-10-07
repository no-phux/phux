//! A real `ServerRuntime` listening on QUIC, for the production-path tests.

#![allow(unreachable_pub, reason = "items shared by several test binaries")]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, QuicDial};
use phux_protocol::wire::RemoteListenerTransport;
use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

pub const STEP_DEADLINE: Duration = Duration::from_secs(15);

/// The TLS/upload configuration a QUIC server reads: a fresh self-signed
/// pair at `cert`/`key`, and `upload_dir` when given. Nothing else is set,
/// so the server never inherits the test process's `PHUX_*` environment.
pub fn quic_env(cert: &Path, key: &Path, upload_dir: Option<&Path>) -> ServerEnv {
    phux_server::transport::tls::ensure_self_signed(cert, key).expect("provision QUIC cert");
    ServerEnv {
        tls_cert: Some(cert.to_path_buf()),
        tls_key: Some(key.to_path_buf()),
        upload_dir: upload_dir.map(Path::to_path_buf),
        ..ServerEnv::default()
    }
}

pub struct Server {
    /// The address the QUIC listener actually bound.
    pub quic_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<Result<(), phux_server::ServerError>>>,
}

impl Server {
    /// Run `config` on the current `LocalSet`, listening on a loopback QUIC
    /// port the kernel picks; [`Self::quic_addr`] is the one it bound.
    pub async fn start(config: ServerConfig) -> Self {
        let socket = config.socket_path.clone();
        let (shutdown, stopped) = oneshot::channel();
        let handle = tokio::task::spawn_local(async move {
            ServerRuntime::new(config)
                .listen_quic(SocketAddr::from(([127, 0, 0, 1], 0)))
                .run_async(async move {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            quic_addr: bound_quic_addr(&socket).await,
            shutdown: Some(shutdown),
            handle: Some(handle),
        }
    }

    pub async fn stop(mut self) {
        self.shutdown.take().unwrap().send(()).ok();
        timeout(STEP_DEADLINE, self.handle.take().unwrap())
            .await
            .expect("server shutdown timed out")
            .expect("server task panicked")
            .expect("server shutdown failed");
    }
}

/// A server config with one pre-seeded PTY session named `session`.
pub fn seeded_config(socket: PathBuf, session: &str) -> ServerConfig {
    ServerConfig {
        socket_path: socket,
        pre_seeded_session: Some(session.to_owned()),
        seed_with_pty: true,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    }
}

/// The QUIC address the server on `socket` reports bound in its `GET_STATE`
/// listener report, retrying until the socket answers.
async fn bound_quic_addr(socket: &Path) -> SocketAddr {
    let deadline = Instant::now() + STEP_DEADLINE;
    let view = loop {
        match phux_client::state::get_state(socket).await {
            Ok(view) => break view,
            Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(25)).await,
            Err(error) => panic!("server never answered GET_STATE: {error}"),
        }
    };
    let slot = view
        .snapshot()
        .listeners()
        .and_then(|report| {
            report
                .listeners
                .iter()
                .find(|slot| slot.transport == RemoteListenerTransport::Quic)
        })
        .cloned()
        .expect("the server reported no QUIC listener");
    assert!(slot.bound, "the QUIC listener did not bind: {slot:?}");
    slot.addr
        .as_deref()
        .and_then(|addr| addr.parse().ok())
        .unwrap_or_else(|| panic!("the QUIC listener reported no address: {slot:?}"))
}

/// Dial the production QUIC path, retrying until the listener is up.
pub async fn dial(addr: SocketAddr) -> Connection {
    let dial = QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: None,
        trust: CertTrust::SkipVerify,
        identity: None,
        inner: None,
    };
    let deadline = Instant::now() + STEP_DEADLINE;
    loop {
        match Connection::connect_quic(&dial).await {
            Ok(connection) => return connection,
            Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(25)).await,
            Err(error) => panic!("QUIC server did not become ready: {error}"),
        }
    }
}
