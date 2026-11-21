//! Reverse enrollment (ADR-0182, phux-lxov.3): a satellite with no listener
//! at all enrolls with a ticket the hub minted, dials the hub's QUIC
//! listener under `phux-satellite/1`, and the hub's link supervisor adopts
//! that connection as the satellite's link. The satellite redials after the
//! hub comes back, and revoking its credential ends the link.

use std::net::{Ipv4Addr, SocketAddr};

use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_server::uplink::{JoinCode, join};
use phux_server::workload::{WorkloadPaths, WorkloadRegistry, tickets};

use super::mixed_window::spawn_cat;
use super::*;

mod satellite_mail;

/// A hub in `paired` mode with a loopback QUIC listener and one
/// `inbound://` satellite named `sat`; its workload authority lives in `dir`.
pub(super) struct PairedHub {
    shutdown: oneshot::Sender<()>,
    task: ServerTask,
    pub(super) quic: SocketAddr,
}

pub(super) use crate::relay_common::private_dir;

pub(super) fn hub_paths(dir: &std::path::Path) -> WorkloadPaths {
    WorkloadPaths::with_overrides(
        Some(dir.join("workload-ca.pem")),
        Some(dir.join("workload-ca.key")),
        Some(dir.join("workload-keys")),
    )
}

fn inbound_entry(name: &str) -> SatelliteConfigEntry {
    SatelliteConfigEntry {
        endpoint: "inbound://".to_owned(),
        ..satellite_entry(name, 0)
    }
}

impl PairedHub {
    /// Boot the hub on `port` (0 for any), its certificate issued by the
    /// workload CA in `dir`.
    async fn boot(dir: &std::path::Path, port: u16) -> Self {
        Self::boot_with(dir, port, None).await
    }

    /// [`Self::boot`], answering satellites' mail from `work_store`.
    pub(super) async fn boot_with(
        dir: &std::path::Path,
        port: u16,
        work_store: Option<Box<phux_work::Client>>,
    ) -> Self {
        let paths = hub_paths(dir);
        let (cert, key) = (dir.join("hub-cert.pem"), dir.join("hub-key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("hub identity");
        let socket = dir.join("hub.sock");
        let cfg = ServerConfig {
            socket_path: socket.clone(),
            policy_mode: Some(phux_config::PolicyMode::Paired),
            env: phux_server::ServerEnv {
                ws_tokens: Some(dir.join("remote-tokens")),
                tls_cert: Some(cert),
                tls_key: Some(key),
                workload_ca: Some(paths.ca_cert),
                workload_ca_key: Some(paths.ca_key),
                workload_keys: Some(paths.registry),
                ..phux_server::ServerEnv::default()
            },
            work_store,
            ..ServerConfig::with_default_socket()
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        // A stopped hub's QUIC endpoint drains its closed connections before
        // it releases the port.
        let deadline = Instant::now() + STEP_DEADLINE;
        while port != 0 && std::net::UdpSocket::bind(addr).is_err() {
            assert!(Instant::now() < deadline, "port {port} never freed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (shutdown, task) = run_runtime(
            terminal_server(cfg)
                .listen_quic(addr)
                .hub(vec![inbound_entry("sat")]),
        );
        let quic = bound_listener_addr(&socket, RemoteListenerTransport::Quic).await;
        Self {
            shutdown,
            task,
            quic,
        }
    }

    pub(super) async fn stop(self) {
        stop(self.shutdown, self.task).await;
    }
}

/// A satellite with no remote listener whose only way out is its uplink.
fn spawn_uplinked_satellite(
    socket_path: PathBuf,
    uplink: phux_config::UplinkConfigEntry,
) -> (oneshot::Sender<()>, ServerTask) {
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some("sat-session".to_owned()),
        ..ServerConfig::with_default_socket()
    };
    run_runtime(terminal_server(cfg).uplinks(vec![uplink]))
}

pub(super) fn mint(dir: &std::path::Path, ticket_seconds: i64) -> tickets::MintedTicket {
    tickets::mint_satellite_ticket(
        &tickets::tickets_path(&hub_paths(dir).registry),
        "sat",
        tickets::DEFAULT_CREDENTIAL_SECONDS,
        ticket_seconds,
    )
    .expect("mint a satellite ticket")
}

pub(super) fn code(
    hub: &PairedHub,
    dir: &std::path::Path,
    ticket: &tickets::MintedTicket,
) -> JoinCode {
    let ca = phux_server::workload::ca_fingerprint(&hub_paths(dir).ca_cert).expect("CA pin");
    JoinCode {
        hub: hub.quic.to_string(),
        server_name: None,
        ca,
        name: "sat".to_owned(),
        ticket: ticket.secret_hex.clone(),
    }
}

/// Retry spawning on the satellite until the hub answers `SatelliteUnreachable`.
async fn until_unreachable(hub: &mut UnixStream, first_request_id: u32) {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut request_id = first_request_id;
    loop {
        let spawn = on_satellite("sat", Spawn::command(&["/bin/cat"]));
        match spawn_resource(hub, request_id, spawn).await {
            SpawnResult::Err(SpawnError::SatelliteUnreachable(_)) => return,
            other => assert!(Instant::now() < deadline, "the link outlived it: {other:?}"),
        }
        request_id += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A direct dial of the hub's QUIC listener, pinning `ca` and presenting
/// `identity`.
fn hub_dial(quic: SocketAddr, ca: &str, identity: TlsClientIdentity) -> QuicDial {
    QuicDial {
        addr: quic,
        server_name: "127.0.0.1".to_owned(),
        token: None,
        trust: CertTrust::Authority { ca: ca.to_owned() },
        identity: Some(identity),
        inner: None,
    }
}

/// HELLO over the hub's terminal ALPN presenting `identity`: what came back,
/// or why the connection ended.
async fn terminal_hello(
    quic: SocketAddr,
    ca: &str,
    identity: TlsClientIdentity,
) -> Result<FrameKind, DialError> {
    let dial = hub_dial(quic, ca, identity);
    crate::relay_common::quic_hello(&dial, "reverse-enrollment").await
}

#[test]
fn a_listenerless_satellite_enrolls_dials_out_and_is_adopted() {
    phux_server_testkit::run_local(async {
        let dir = private_dir();
        let d = dir.path();
        let hub = PairedHub::boot(d, 0).await;

        // Ticket expiry, revocation and single use are pinned by
        // `workload::tickets`; this is the wire path a live ticket takes.
        let ticket = mint(d, 600);
        let join_code = code(&hub, d, &ticket);
        let joined = join(&join_code, &d.join("sat-state"))
            .await
            .expect("the ticket enrolls the satellite");
        let registry = WorkloadRegistry::load(&hub_paths(d).registry).unwrap();
        let record = registry
            .lookup_satellite(&joined.credential)
            .expect("the hub recorded a satellite credential");
        assert_eq!(record.satellite.as_deref(), Some("sat"));
        assert!(record.scopes.is_empty(), "a satellite carries no ceiling");
        assert!(
            registry.lookup(&joined.credential).is_none(),
            "a satellite credential is never a terminal credential"
        );

        // The satellite credential cannot attach as a terminal client.
        let attached = terminal_hello(
            hub.quic,
            &join_code.ca,
            TlsClientIdentity::PemFiles {
                certificate: joined.entry.client_cert.clone(),
                private_key: joined.entry.client_key.clone(),
            },
        )
        .await;
        assert!(
            matches!(attached, Err(DialError::AuthRefused(_))),
            "a satellite credential on the terminal ALPN: {attached:?}"
        );

        // No listener anywhere on the satellite: it is reachable only
        // through the link it dialed. Its terminals answer through the hub.
        let (sat_shutdown, sat_task) =
            spawn_uplinked_satellite(d.join("sat.sock"), joined.entry.clone());
        let mut client = hub_client(&d.join("hub.sock")).await;
        let cat = spawn_cat(&mut client, 100, Some("sat")).await;
        assert!(matches!(
            get_screen_via_hub(&mut client, 101, cat.clone()).await,
            CommandResult::OkWith(CommandValue::Json(ref json)) if json.contains("\"cols\"")
        ));
        let (result, _) = command_via_hub(&mut client, 102, focus_gained(cat)).await;
        assert_eq!(result, CommandResult::Ok, "input reaches the satellite");

        // The hub goes away and comes back on the same port: the satellite
        // redials on its own and the new hub adopts it.
        drop(client);
        let port = hub.quic.port();
        hub.stop().await;
        let hub = PairedHub::boot(d, port).await;
        let mut client = hub_client(&d.join("hub.sock")).await;
        let again = spawn_cat(&mut client, 200, Some("sat")).await;
        assert!(matches!(
            get_screen_via_hub(&mut client, 201, again).await,
            CommandResult::OkWith(CommandValue::Json(_))
        ));

        // Revoking the credential ends the live link, and the satellite's
        // redials are refused from then on.
        WorkloadRegistry::revoke(&hub_paths(d).registry, &joined.credential).unwrap();
        until_unreachable(&mut client, 300).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        until_unreachable(&mut client, 400).await;

        drop(client);
        stop(sat_shutdown, sat_task).await;
        hub.stop().await;
    });
}

/// A re-enrollment of the same name revokes the credential it replaces, and
/// a hub with no `inbound://` entry for a name refuses its satellite.
#[test]
fn one_live_credential_per_name_and_unknown_names_are_refused() {
    phux_server_testkit::run_local(async {
        let dir = private_dir();
        let d = dir.path();
        let hub = PairedHub::boot(d, 0).await;
        let first = join(&code(&hub, d, &mint(d, 600)), &d.join("one"))
            .await
            .expect("first enrollment");
        let second = join(&code(&hub, d, &mint(d, 600)), &d.join("two"))
            .await
            .expect("second enrollment");
        let registry = WorkloadRegistry::load(&hub_paths(d).registry).unwrap();
        assert!(registry.lookup_satellite(&first.credential).is_none());
        assert!(registry.lookup_satellite(&second.credential).is_some());

        // A credential for a name the hub's table lacks: enrolled, but never
        // adopted.
        let stray = tickets::mint_satellite_ticket(
            &tickets::tickets_path(&hub_paths(d).registry),
            "elsewhere",
            tickets::DEFAULT_CREDENTIAL_SECONDS,
            600,
        )
        .unwrap();
        let mut stray_code = code(&hub, d, &stray);
        stray_code.name = "elsewhere".to_owned();
        let stray = join(&stray_code, &d.join("stray")).await.expect("enrolls");
        let identity = TlsClientIdentity::PemFiles {
            certificate: stray.entry.client_cert.clone(),
            private_key: stray.entry.client_key.clone(),
        };
        let plan = hub_dial(hub.quic, &stray_code.ca, identity);
        let (_endpoint, conn) =
            phux_dial::quic::connect_with_alpn(&plan, phux_protocol::policy::SATELLITE_ALPN)
                .await
                .expect("the handshake itself completes");
        let closed = tokio::time::timeout(STEP_DEADLINE, conn.closed())
            .await
            .expect("the hub closes an unknown satellite");
        assert!(
            matches!(
                phux_dial::quic::close_error(&closed),
                DialError::AuthRefused(_)
            ),
            "{closed:?}"
        );

        hub.stop().await;
    });
}
