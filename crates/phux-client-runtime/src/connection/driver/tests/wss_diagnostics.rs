//! Real TCP, pinned TLS and held-key callbacks under the production driver.
use super::diagnostics::{Recorded, inventory_attempt};
use super::*;
use crate::enroll::{DeviceSigner, SoftwareSigner, held_identity};
use phux_dial::{TlsClientIdentity, ws::ClientStream};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

mod signing;

struct PublicPoint(Vec<u8>);

impl rcgen::PublicKeyData for PublicPoint {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }
    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

fn client_chain(signer: &dyn DeviceSigner) -> (rustls::pki_types::CertificateDer<'static>, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    let ca = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_der(ca.der(), key).unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    let cert = params
        .signed_by(&PublicPoint(signer.public_point()), &issuer)
        .unwrap();
    (ca.der().clone(), cert.pem() + &ca.pem())
}

#[derive(Clone, Copy)]
enum Stall {
    Tls,
    Upgrade,
}

async fn read_upgrade(stream: &mut ClientStream) {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        assert!(request.len() < 8192, "bounded upgrade request");
        request.push(stream.read_u8().await.unwrap());
    }
    assert!(request.starts_with(b"GET /"));
    assert!(
        request
            .windows(b"Upgrade: websocket".len())
            .any(|bytes| bytes.eq_ignore_ascii_case(b"Upgrade: websocket"))
    );
}

async fn stalled_peer(
    stall: Stall,
    signer: Arc<dyn DeviceSigner>,
    connections: usize,
) -> (
    Target,
    impl std::future::Future<Output = usize>,
    mpsc::Receiver<()>,
) {
    let directory = tempfile::tempdir().unwrap();
    let cert = directory.path().join("cert.pem");
    let key = directory.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed_for(&cert, &key, &["localhost".to_owned()])
        .unwrap();
    let (ca, chain) = client_chain(signer.as_ref());
    let mut acceptor = phux_server::transport::tls::acceptor_from_pem(&cert, &key).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::clone(&provider),
    )
    .build()
    .unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::clone(&acceptor.config().cert_resolver));
    acceptor = Arc::new(tls).into();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut target = Target::ws(format!("wss://{}/", listener.local_addr().unwrap()));
    target.name = "private-host-sentinel".to_owned();
    target.cert_fingerprint = Some(phux_dial::cert::cert_fingerprint(&cert).unwrap());
    target.token = Some("abcd".to_owned());
    target.client_identity = TlsClientIdentity::Held(held_identity(&chain, signer).unwrap());
    let (ready, receiver) = mpsc::channel(connections);
    let peer = async move {
        let _directory = directory;
        for _ in 0..connections {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut stream: ClientStream = match stall {
                Stall::Tls => {
                    // Consume a real TLS ClientHello record, not a virtual sleep.
                    let mut header = [0; 5];
                    tcp.read_exact(&mut header).await.unwrap();
                    assert_eq!(header[0], 22, "TLS handshake record");
                    let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
                    assert!(length > 0 && length <= 16384);
                    let mut hello = vec![0; length];
                    tcp.read_exact(&mut hello).await.unwrap();
                    assert_eq!(hello[0], 1, "ClientHello");
                    Box::new(tcp)
                }
                Stall::Upgrade => {
                    let mut stream: ClientStream = Box::new(acceptor.accept(tcp).await.unwrap());
                    read_upgrade(&mut stream).await;
                    stream
                }
            };
            ready.send(()).await.unwrap();
            // Cancellation must close this original socket before the next
            // attempt can reach the fixture's barrier.
            let mut tail = Vec::new();
            let end = stream.read_to_end(&mut tail).await;
            assert!(tail.len() < 8192);
            if let Err(error) = end {
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ));
            }
        }
        connections
    };
    (target, peer, receiver)
}

fn rows(recorded: &Recorded) -> Vec<serde_json::Value> {
    recorded.0.lock().unwrap().clone()
}

async fn join_owned_peer<T>(
    mut peer: tokio::task::JoinHandle<T>,
    budget: Duration,
) -> Result<T, String> {
    if let Ok(result) = tokio::time::timeout(budget, &mut peer).await {
        return result.map_err(|error| format!("owned peer failed: {error}"));
    }
    peer.abort();
    let _ = peer.await;
    Err("owned peer did not finish before its join deadline".to_owned())
}

#[tokio::test(start_paused = true)]
async fn expired_peer_join_aborts_and_reaps_the_owned_task_before_returning_failure() {
    struct OnDrop(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let owned = Arc::clone(&dropped);
    let (started, ready) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let _drop = OnDrop(owned);
        started.send(()).unwrap();
        std::future::pending::<usize>().await
    });
    ready.await.unwrap();
    let result = join_owned_peer(peer, Duration::from_millis(10)).await;
    assert!(result.unwrap_err().contains("join deadline"));
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "abort is awaited, not merely requested"
    );
}

async fn advance_before_deadline(deadline: Instant) {
    // Real-I/O setup leaves a fractional-millisecond origin. Tokio's timer
    // wheel rounds it up. First prove the operation is still pending just
    // before expiry, then the caller crosses that one timer tick.
    tokio::time::advance(
        deadline
            .saturating_duration_since(Instant::now())
            .saturating_sub(Duration::from_millis(1)),
    )
    .await;
}

async fn await_socket_stage(
    running: Pin<&mut impl std::future::Future<Output = ()>>,
    ready: &mut mpsc::Receiver<()>,
    budget: Duration,
) {
    tokio::select! {
        () = running => panic!("the pending fixture has not answered"),
        stage = ready.recv() => assert_eq!(stage, Some(())),
        () = tokio::time::sleep(budget) => panic!("real socket stage did not arrive"),
    }
}

async fn expire_first_dial(
    mut running: Pin<&mut impl std::future::Future<Output = ()>>,
    first: AttemptTrace,
    options: ConnectOptions,
    recorded: &Recorded,
) {
    tokio::time::pause();
    advance_before_deadline(first.started + options.dial_timeout).await;
    assert!(running.as_mut().now_or_never().is_none());
    assert!(
        !rows(recorded)
            .iter()
            .any(|row| row["phase"] == "dial_deadline")
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    assert!(
        running.as_mut().now_or_never().is_none(),
        "the first deadline walks backoff"
    );
    let deadline = rows(recorded)
        .into_iter()
        .find(|row| row["phase"] == "dial_deadline")
        .unwrap();
    assert!((15_000..=15_001).contains(&deadline["elapsed_ms"].as_u64().unwrap()));
    assert_eq!(deadline["attempt"], 1);
    tokio::time::advance(options.ladder.floor).await;
    assert!(running.as_mut().now_or_never().is_none());
    tokio::time::resume();
}

async fn expire_initial_budget(
    mut running: Pin<&mut impl std::future::Future<Output = ()>>,
    first: AttemptTrace,
    options: ConnectOptions,
) {
    tokio::time::pause();
    advance_before_deadline(first.started + options.initial_budget).await;
    assert!(
        running.as_mut().now_or_never().is_none(),
        "the original 18s budget has not expired"
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    assert_eq!(running.as_mut().now_or_never(), Some(()));
    tokio::time::resume();
}

async fn stage_deadlines(stall: Stall) {
    let recorded = Recorded::default();
    let _subscriber = tracing::subscriber::set_default(recorded.clone());
    let signer: Arc<dyn DeviceSigner> = Arc::new(SoftwareSigner::generate());
    let (target, peer, mut ready) = stalled_peer(stall, signer, 2).await;
    let peer = tokio::spawn(peer);
    let fixture = WriteFixture::new(1024);
    let shared = Arc::clone(&fixture.shared);
    let wake: Wake = Arc::new(|| {});
    let options = ConnectOptions::default();
    assert_eq!(options.dial_timeout, Duration::from_secs(15));
    assert_eq!(options.initial_budget, Duration::from_secs(18));
    let mut running = Box::pin(run_session(
        target,
        options,
        Arc::clone(&shared),
        fixture.signals,
        wake,
    ));
    await_socket_stage(running.as_mut(), &mut ready, Duration::from_secs(5)).await;
    let first = recorded.1.lock().unwrap()[0];
    let stage = match stall {
        Stall::Tls => "ws_tls_handshake",
        Stall::Upgrade => "ws_upgrade",
    };
    assert_eq!(rows(&recorded).last().unwrap()["phase"], stage);
    expire_first_dial(running.as_mut(), first, options, &recorded).await;
    // Real sockets/crypto run only with the real clock. The second barrier
    // also acknowledges that cancellation closed the first socket.
    await_socket_stage(running.as_mut(), &mut ready, Duration::from_secs(2)).await;
    expire_initial_budget(running.as_mut(), first, options).await;
    assert_eq!(
        join_owned_peer(peer, Duration::from_secs(3)).await.unwrap(),
        2,
        "both expired sockets were closed"
    );
    assert_eq!(lock(&shared).status(), Status::Failed);
    let failure = lock(&shared).last_error().unwrap().to_owned();
    assert!(
        failure.contains("15s"),
        "the original dial error remains primary"
    );
    assert_stalled_events(&rows(&recorded), first.connection, stall);
    tokio::time::pause();
    let replacement = inventory_attempt(AttemptTrace::connection(), true).await;
    tokio::time::resume();
    assert!(lock(&replacement).attached_once());
    assert_eq!(lock(&shared).status(), Status::Failed);
    assert_eq!(lock(&shared).last_error(), Some(failure.as_str()));
    assert!(
        !serde_json::to_string(&rows(&recorded))
            .unwrap()
            .contains("private-")
    );
}

fn assert_stalled_events(events: &[serde_json::Value], connection: u64, stall: Stall) {
    let initial = events
        .iter()
        .find(|row| row["phase"] == "initial_deadline")
        .unwrap();
    assert_eq!(initial["attempt"], 2);
    assert_eq!(initial["connection"], connection);
    assert!(
        initial["elapsed_ms"].as_u64().unwrap() < 3000,
        "only the remaining 18s budget"
    );
    assert!(!events.iter().any(|row| matches!(
        row["phase"].as_str().unwrap(),
        "ws_upgraded" | "dial_connected" | "hello_sent" | "input_ready" | "input_injected"
    )));
    for attempt in [1, 2] {
        let phases: Vec<_> = events
            .iter()
            .filter(|row| row["attempt"] == attempt)
            .map(|row| row["phase"].as_str().unwrap())
            .collect();
        assert_attempt_order(&phases, stall);
    }
}

fn assert_attempt_order(phases: &[&str], stall: Stall) {
    assert!(phases.starts_with(&[
        "attempt_started",
        "dial_started",
        "dial_plan_started",
        "dial_plan_ready",
        "ws_resolve",
        "ws_tcp_connect",
        "ws_tls_config",
        "ws_tls_handshake",
    ]));
    if matches!(stall, Stall::Upgrade) {
        let started = phases
            .iter()
            .position(|phase| *phase == "tls_sign_started")
            .unwrap();
        let returned = phases
            .iter()
            .position(|phase| *phase == "tls_sign_returned")
            .unwrap();
        let upgrade = phases
            .iter()
            .position(|phase| *phase == "ws_upgrade")
            .unwrap();
        assert!(started < returned && returned < upgrade);
    } else {
        assert!(!phases.contains(&"tls_sign_started") && !phases.contains(&"ws_upgrade"));
    }
}

#[tokio::test]
async fn real_tls_stall_preserves_the_production_dial_and_initial_deadlines() {
    stage_deadlines(Stall::Tls).await;
}

#[tokio::test]
async fn real_mtls_upgrade_stall_identifies_signing_and_preserves_deadlines() {
    stage_deadlines(Stall::Upgrade).await;
}
