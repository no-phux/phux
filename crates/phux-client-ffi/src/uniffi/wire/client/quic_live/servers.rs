//! The real servers the QUIC live cases dial: a loopback listener, a
//! `paired` one, and a relay fronting a `paired` server.

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};

use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use tokio::sync::oneshot;

use super::*;

/// A real server on its own thread, listening on loopback QUIC at `addr`.
pub(super) struct QuicServer {
    pub(super) addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    pub(super) dir: tempfile::TempDir,
}

impl QuicServer {
    pub(super) fn start() -> Self {
        Self::start_with(false)
    }

    /// With `authority`, the certificate is the one a server provisions
    /// itself: issued by its workload CA, presented after the leaf
    /// (ADR-0153). The server runs an embedder engine (ADR-0072): these
    /// cases are about the dial, not about who may act.
    pub(super) fn start_with(authority: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        if authority {
            std::fs::set_permissions(
                dir.path(),
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .expect("owner-only state directory");
            let paths = phux_server::workload::WorkloadPaths::with_overrides(
                Some(dir.path().join("workload-ca.pem")),
                Some(dir.path().join("workload-ca.key")),
                Some(dir.path().join("workload-keys")),
            );
            phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
                .expect("cert");
        } else {
            phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("cert");
        }
        // The runtime's reconnect ladder covers the listener's startup, so a
        // reserved-then-released port is enough; no readiness probe needed.
        let addr = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("reserve a loopback UDP port");
        let mut config = ServerConfig {
            socket_path: dir.path().join("phux.sock"),
            pre_seeded_session: Some(SESSION.to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: ServerEnv {
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        phux_server_testkit::embedder::embed_owner_engine(&mut config);
        let (shutdown, stopped) = oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                ServerRuntime::new(config)
                    .listen_quic(addr)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
            });
        });
        Self {
            addr,
            shutdown: Some(shutdown),
            thread: Some(thread),
            dir,
        }
    }

    pub(super) fn cert(&self) -> std::path::PathBuf {
        self.dir.path().join("cert.pem")
    }

    /// A server in `paired` mode: every QUIC connection must present an
    /// enrolled workload certificate (ADR-0116), and the listener offers the
    /// enrollment ALPN (ADR-0154). Its authority lives in its own directory.
    pub(super) fn start_paired() -> (Self, phux_server::workload::WorkloadPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("owner-only state directory");
        let paths = phux_server::workload::WorkloadPaths::with_overrides(
            Some(dir.path().join("workload-ca.pem")),
            Some(dir.path().join("workload-ca.key")),
            Some(dir.path().join("workload-keys")),
        );
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("cert");
        let addr = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("reserve a loopback UDP port");
        let config = ServerConfig {
            socket_path: dir.path().join("phux.sock"),
            pre_seeded_session: Some(SESSION.to_owned()),
            seed_with_pty: false,
            seed_command: None,
            policy_mode: Some(phux_config::PolicyMode::Paired),
            env: ServerEnv {
                tls_cert: Some(cert),
                tls_key: Some(key),
                workload_ca: Some(paths.ca_cert.clone()),
                workload_ca_key: Some(paths.ca_key.clone()),
                workload_keys: Some(paths.registry.clone()),
                ..ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        let (shutdown, stopped) = oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                ServerRuntime::new(config)
                    .listen_quic(addr)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
            });
        });
        (
            Self {
                addr,
                shutdown: Some(shutdown),
                thread: Some(thread),
                dir,
            },
            paths,
        )
    }
}

/// A relay, and behind it a `paired` server that reaches it only through its
/// `[[connector]]`: the phone dials the relay and meets the server end to
/// end inside the relayed stream (ADR-0154 item 5).
pub(super) struct RelayedServer {
    relay: SocketAddr,
    relay_pin: String,
    pub(super) authority: String,
    pub(super) paths: phux_server::workload::WorkloadPaths,
    token: String,
    pub(super) observed_sni: Arc<Mutex<Vec<String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

pub(super) const ROUTE: &str = "phone-route";

impl RelayedServer {
    pub(super) fn start() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("owner-only");
        let d = dir.path().to_path_buf();
        let write_secret = |name: &str, body: &str| {
            std::fs::write(d.join(name), body).expect("secret");
            std::fs::set_permissions(d.join(name), std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        };
        let tunnel = "31".repeat(32);
        std::fs::write(d.join("relay-tokens"), format!("{tunnel} {ROUTE}\n")).expect("route");
        write_secret("connector-token", &format!("{tunnel}\n"));
        let token = phux_server::auth::mint_token(&d.join("consumer-tokens"))
            .expect("consumer credential")
            .secret()
            .to_owned();
        let paths = phux_server::workload::WorkloadPaths::with_overrides(
            Some(d.join("workload-ca.pem")),
            Some(d.join("workload-ca.key")),
            Some(d.join("workload-keys")),
        );
        let (cert, key) = (d.join("cert.pem"), d.join("key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("server identity");
        let authority = phux_server::transport::tls::presented_authority(&cert)
            .expect("chain")
            .expect("issued by the CA");
        let (shutdown, stopped) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let env = ServerEnv {
            ws_tokens: Some(d.join("consumer-tokens")),
            tls_cert: Some(cert),
            tls_key: Some(key),
            workload_ca: Some(paths.ca_cert.clone()),
            workload_ca_key: Some(paths.ca_key.clone()),
            workload_keys: Some(paths.registry.clone()),
            ..ServerEnv::default()
        };
        let observed_sni = Arc::new(Mutex::new(Vec::new()));
        let state = d.clone();
        let relay_sni = Arc::clone(&observed_sni);
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                let relay = phux_server_testkit::relay::RelayHarness::start_observing(
                    phux_relay::RelayConfig {
                        max_conns: 16,
                        ..phux_relay::config_in(SocketAddr::from(([127, 0, 0, 1], 0)), &state)
                    },
                    relay_sni,
                );
                let pin =
                    phux_relay::cert_fingerprint(&state.join("relay-cert.pem")).expect("relay pin");
                let connector = phux_config::ConnectorConfigEntry {
                    relay: relay.addr.to_string(),
                    token_file: Some(state.join("connector-token")),
                    cert_fingerprint: Some(pin.clone()),
                };
                ready_tx.send((relay.addr, pin)).expect("ready");
                let config = ServerConfig {
                    socket_path: state.join("phux.sock"),
                    policy_mode: Some(phux_config::PolicyMode::Paired),
                    pre_seeded_session: Some(SESSION.to_owned()),
                    seed_with_pty: false,
                    seed_command: None,
                    env,
                    ..ServerConfig::with_default_socket()
                };
                ServerRuntime::new(config)
                    .connectors(vec![connector], None)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
                relay.stop().await;
            });
        });
        let (relay, relay_pin) = ready_rx.recv().expect("relay up");
        Self {
            relay,
            relay_pin,
            authority,
            paths,
            token,
            observed_sni,
            shutdown: Some(shutdown),
            thread: Some(thread),
            _dir: dir,
        }
    }

    pub(super) fn client(&self) -> Arc<RemoteClient> {
        let client = RemoteClient::new_routed(
            format!("quic://{}", self.relay),
            80,
            24,
            Some(self.relay_pin.clone()),
            Some(self.token.clone()),
            ROUTE.to_owned(),
        )
        .expect("valid relay route");
        client.set_authority_pin(Some(self.authority.clone()));
        client
    }
}

impl Drop for RelayedServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A device key for the binding, in software (the phone's is in its
/// keystore; the binding cannot tell).
pub(super) struct TestKey(phux_client_runtime::enroll::SoftwareSigner);

impl DeviceKey for TestKey {
    fn public_point(&self) -> Vec<u8> {
        phux_client_runtime::enroll::DeviceSigner::public_point(&self.0)
    }

    fn sign(&self, message: Vec<u8>) -> Result<Vec<u8>, DeviceKeyError> {
        phux_client_runtime::enroll::DeviceSigner::sign(&self.0, &message)
            .map_err(|reason| DeviceKeyError::Failed { reason })
    }
}

pub(super) fn test_key() -> Arc<dyn DeviceKey> {
    Arc::new(TestKey(
        phux_client_runtime::enroll::SoftwareSigner::generate(),
    ))
}

pub(super) fn mint(paths: &phux_server::workload::WorkloadPaths) -> String {
    phux_server::workload::tickets::mint_ticket(
        &phux_server::workload::tickets::tickets_path(&paths.registry),
        vec!["inventory,observe,create,bind,input,signal@global".to_owned()],
        3600,
        600,
    )
    .expect("ticket")
    .secret_hex
}

impl Drop for QuicServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
