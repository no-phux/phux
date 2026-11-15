//! Workload identity through a relay (ADR-0154 item 5): the consumer runs
//! TLS end to end with the server inside the relayed stream, so its
//! certificate reaches the server although the relay terminates the outer
//! TLS. That session is the only way in, in every posture (ADR-0180).
//!
//! ```text
//! consumer ==TLS(relay pin)==> relay <== connector (in server)
//!          \_____ TLS(server CA pin, client cert) ______/
//! ```

use std::time::{Duration, Instant};

use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_protocol::wire::frame::FrameKind;
use phux_server::workload::WorkloadPaths;
use phux_server_testkit::relay::RelayHarness;
use tempfile::TempDir;
use tokio::time::sleep;

use crate::relay_common::{
    RelayedServer, loopback, quic_hello, relay_config, relay_pin, write_private,
};

const ROUTE: &str = "inner-test";
const TUNNEL_TOKEN: [u8; 32] = [0x31; 32];

struct Topology {
    _dir: TempDir,
    relay: RelayHarness,
    relay_pin: String,
    server: RelayedServer,
}

impl Topology {
    /// A relay, and behind it a server whose certificate its workload CA
    /// issued; `paired` puts the server in `paired` policy mode, and
    /// otherwise it runs an embedder engine (ADR-0072), the only way a
    /// connector serves without `paired` (ADR-0179).
    fn start(paired: bool) -> Self {
        let dir = TempDir::new().expect("tempdir");
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("owner-only");
        let d = dir.path();
        let relay = RelayHarness::start(relay_config(d, loopback(), ROUTE, &TUNNEL_TOKEN));
        let relay_pin = relay_pin(d);
        let server =
            RelayedServer::start(d, relay.addr, &relay_pin, &TUNNEL_TOKEN, |cfg, paths| {
                if paired {
                    // Naming the authority without `paired` refuses to start
                    // (workload-auth §8), so only the paired server names it.
                    cfg.policy_mode = Some(phux_config::PolicyMode::Paired);
                    cfg.env.workload_ca = Some(paths.ca_cert.clone());
                    cfg.env.workload_ca_key = Some(paths.ca_key.clone());
                    cfg.env.workload_keys = Some(paths.registry.clone());
                } else {
                    phux_server_testkit::embedder::embed_owner_engine(cfg);
                }
            });
        Self {
            _dir: dir,
            relay,
            relay_pin,
            server,
        }
    }

    fn plan(&self, inner: Option<phux_dial::quic::InnerTls>) -> QuicDial {
        let token = Some(self.server.bearer.clone());
        crate::relay_common::plan(self.relay.addr, &self.relay_pin, ROUTE, token, inner)
    }

    async fn stop(self) {
        self.server.stop().await;
        self.relay.stop().await;
    }
}

/// The end-to-end session to the server behind [`ROUTE`], pinning `ca`.
fn inner(ca: &str, identity: TlsClientIdentity) -> phux_dial::quic::InnerTls {
    crate::relay_common::inner(ca, identity, ROUTE)
}

/// Retry until the connector holds the route.
async fn first_answer(plan: &QuicDial) -> Result<FrameKind, DialError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let answer = quic_hello(plan, "relay-inner-tls").await;
        let route_offline =
            matches!(&answer, Err(DialError::Connect(detail)) if detail.contains("route offline"));
        if !route_offline || Instant::now() > deadline {
            return answer;
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// The connector's first-byte gate refused a plain relayed stream: the
/// route was up, the stream reached the server, and the server reset it on
/// the tunnel with `AUTH_FAILED`. The relay forwards no reset code (it only
/// splices bytes), so the consumer sees the relay end its bridged
/// connection with code 0 (or, should the stream's FIN be read before that
/// close lands, the stream end), after the dial succeeded and before any
/// answer. Not a timeout ("no answer"), not `ROUTE_OFFLINE`, not a dial
/// failure.
fn assert_plain_refused(answer: &Result<FrameKind, DialError>) {
    assert!(
        matches!(
            answer,
            Err(DialError::Connect(detail))
                if detail == "closed by peer: 0" || detail == "early eof"
        ),
        "a plain bridged consumer must be refused by the connector gate, got {answer:?}"
    );
}

/// Wait until the server's connector holds the route.
async fn wait_for_route(t: &Topology) {
    let answer = first_answer(&t.plan(None)).await;
    assert!(
        !matches!(&answer, Err(DialError::Connect(detail)) if detail.contains("route offline")),
        "the connector never held the route: {answer:?}"
    );
}

fn ticket(paths: &WorkloadPaths) -> Vec<u8> {
    let minted = phux_server::workload::tickets::mint_ticket(
        &phux_server::workload::tickets::tickets_path(&paths.registry),
        vec![phux_server::workload::PAIRED_DEVICE_SCOPE.to_owned()],
        3600,
        600,
    )
    .expect("ticket");
    hex::decode(minted.secret_hex).expect("hex")
}

/// Even under an embedder engine, which admits a bearer alone, the plain
/// relayed stream is refused (ADR-0180: the relay would read it); an inner
/// session pinning the server's CA is admitted, and a wrong CA is refused by
/// name.
#[test]
fn an_inner_session_reaches_the_server_and_a_wrong_ca_is_refused() {
    phux_server_testkit::run_local(async {
        let t = Topology::start(false);
        wait_for_route(&t).await;
        // In every posture.
        assert_plain_refused(&first_answer(&t.plan(None)).await);
        let session = inner(&t.server.authority, TlsClientIdentity::None);
        let answer = first_answer(&t.plan(Some(session)))
            .await
            .expect("inner admitted");
        assert!(matches!(answer, FrameKind::HelloOk { .. }), "{answer:?}");

        let wrong = inner(
            &format!("sha256:{}", "0".repeat(64)),
            TlsClientIdentity::None,
        );
        match first_answer(&t.plan(Some(wrong))).await {
            Err(DialError::AuthorityChanged(change)) => {
                assert_eq!(
                    change.presented.as_deref(),
                    Some(t.server.authority.as_str())
                );
            }
            other => panic!("a wrong CA must be refused by name, got {other:?}"),
        }
        t.stop().await;
    });
}

/// Under `paired`: a plain consumer and a certificate-less inner one are
/// refused; a device enrolls through the relay with a ticket and is then
/// admitted with that certificate; the ticket does not enroll twice.
#[test]
fn under_paired_only_an_enrolled_certificate_crosses_the_relay() {
    phux_server_testkit::run_local(async {
        let t = Topology::start(true);
        wait_for_route(&t).await;
        assert_plain_refused(&first_answer(&t.plan(None)).await);
        let bare = inner(&t.server.authority, TlsClientIdentity::None);
        assert!(
            !matches!(
                first_answer(&t.plan(Some(bare))).await,
                Ok(FrameKind::HelloOk { .. })
            ),
            "an inner session without a certificate is refused"
        );

        let key = rcgen::KeyPair::generate().expect("key");
        let csr = rcgen::CertificateParams::new(Vec::<String>::new())
            .expect("params")
            .serialize_request(&key)
            .expect("csr")
            .der()
            .to_vec();
        let ticket = ticket(&t.server.paths);
        let enroll_dial = phux_dial::enroll::EnrollDial {
            addr: t.relay.addr,
            server_name: ROUTE.to_owned(),
            trust: CertTrust::Pinned(t.relay_pin.clone()),
            inner: Some(inner(&t.server.authority, TlsClientIdentity::None)),
        };
        let request = phux_protocol::enroll::Request {
            ticket: ticket.clone(),
            csr: csr.clone(),
        };
        let chain = phux_dial::enroll::enroll(&enroll_dial, &request)
            .await
            .expect("enrolled through the relay");
        phux_dial::enroll::check_issued_chain(
            &chain,
            &rcgen::PublicKeyData::subject_public_key_info(&key),
            Some(&t.server.authority),
        )
        .expect("the reply passes the client checks");
        let replay = phux_dial::enroll::enroll(&enroll_dial, &request).await;
        assert!(
            matches!(replay, Err(DialError::AuthRefused(_))),
            "{replay:?}"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key_path) = (dir.path().join("c.pem"), dir.path().join("c.key"));
        std::fs::write(&cert, &chain).expect("chain");
        write_private(&key_path, key.serialize_pem());
        let enrolled = inner(
            &t.server.authority,
            TlsClientIdentity::PemFiles {
                certificate: cert,
                private_key: key_path,
            },
        );
        let answer = first_answer(&t.plan(Some(enrolled)))
            .await
            .expect("admitted");
        assert!(matches!(answer, FrameKind::HelloOk { .. }), "{answer:?}");
        t.stop().await;
    });
}
