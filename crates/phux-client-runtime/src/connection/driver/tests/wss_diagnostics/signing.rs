//! Held-key task attribution, refusal, ownership and synchronous blocking.
use super::*;

struct RefusingSigner(SoftwareSigner);
impl DeviceSigner for RefusingSigner {
    fn public_point(&self) -> Vec<u8> {
        self.0.public_point()
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        if message == b"private-refused-message" {
            return Err("private-sign-error".to_owned());
        }
        self.0.sign(message)
    }
}

#[tokio::test]
async fn shared_held_signer_is_task_scoped_across_retries_and_refusals() {
    let recorded = Recorded::default();
    let _subscriber = tracing::subscriber::set_default(recorded.clone());
    let signer: Arc<dyn DeviceSigner> = Arc::new(RefusingSigner(SoftwareSigner::generate()));
    let (_, chain) = client_chain(signer.as_ref());
    let identity = held_identity(&chain, Arc::clone(&signer)).unwrap();
    let key = identity.key;
    let offered = [rustls::SignatureScheme::ECDSA_NISTP256_SHA256];
    key.choose_scheme(&offered)
        .unwrap()
        .sign(b"private-outside-scope")
        .unwrap();
    assert!(rows(&recorded).is_empty());
    let first = AttemptTrace::connection();
    let second = AttemptTrace::connection();
    let attempt = |connection, number| {
        let key = Arc::clone(&key);
        async move {
            let trace = AttemptTrace::new(connection, number);
            trace
                .scope(async {
                    tokio::task::yield_now().await;
                    let tls = key.choose_scheme(&offered).unwrap();
                    tls.sign(b"private-ok-message").unwrap();
                    tokio::task::yield_now().await;
                assert!(matches!(tls.sign(b"private-refused-message"),
                    Err(rustls::Error::General(reason)) if reason == "device key: private-sign-error"));
                })
                .await;
        }
    };
    let a = tokio::spawn(attempt(first, 1));
    let b = tokio::spawn(attempt(second, 1));
    a.await.unwrap();
    b.await.unwrap();
    attempt(first, 2).await;
    for (connection, number) in [(first, 1), (second, 1), (first, 2)] {
        let phases: Vec<_> = rows(&recorded)
            .into_iter()
            .filter(|row| row["connection"] == connection && row["attempt"] == number)
            .map(|row| row["phase"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            phases,
            [
                "tls_sign_started",
                "tls_sign_returned",
                "tls_sign_started",
                "tls_sign_returned"
            ]
        );
    }
    let count = rows(&recorded).len();
    key.choose_scheme(&offered)
        .unwrap()
        .sign(b"private-after-scope")
        .unwrap();
    assert_eq!(rows(&recorded).len(), count);
    assert!(
        rows(&recorded)
            .iter()
            .all(|row| row.as_object().unwrap().len() == 4)
    );
    assert!(
        !serde_json::to_string(&rows(&recorded))
            .unwrap()
            .contains("private-")
    );
    assert_eq!(
        Arc::strong_count(&signer),
        2,
        "the held key retains the platform signer, not an attempt"
    );
    drop(key);
    assert_eq!(
        Arc::strong_count(&signer),
        1,
        "no diagnostic observer retains the key"
    );
}

struct BlockingSigner {
    key: SoftwareSigner,
    entered: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl DeviceSigner for BlockingSigner {
    fn public_point(&self) -> Vec<u8> {
        self.key.public_point()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.entered
            .send(())
            .map_err(|_| "fixture watchdog gone".to_owned())?;
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "fixture watchdog did not release".to_owned())?;
        self.key.sign(message)
    }
}

/// The native current-thread runtime cannot preempt a synchronous key callback.
/// This is real wall time and real mTLS, not a paused async-sleep surrogate.
#[tokio::test(flavor = "current_thread")]
async fn synchronous_mtls_signing_can_overshoot_a_timeout_until_external_release() {
    let recorded = Recorded::default();
    let _subscriber = tracing::subscriber::set_default(recorded.clone());
    let (entered, started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let signer: Arc<dyn DeviceSigner> = Arc::new(BlockingSigner {
        key: SoftwareSigner::generate(),
        entered,
        release: Mutex::new(released),
    });
    let (target, peer, _ready) = stalled_peer(Stall::Upgrade, signer, 1).await;
    let peer = tokio::spawn(peer);
    let watch = recorded.clone();
    // Bounded independent watchdog, always released and joined before assertions.
    let watchdog = std::thread::spawn(move || {
        let entered = started.recv_timeout(Duration::from_secs(5)).is_ok();
        if entered {
            std::thread::sleep(Duration::from_millis(1200));
        }
        let before_release = rows(&watch);
        let _ = release.send(());
        (entered, before_release)
    });
    let fixture = WriteFixture::new(1024);
    let shared = Arc::clone(&fixture.shared);
    let options = ConnectOptions {
        dial_timeout: Duration::from_secs(1),
        initial_budget: Duration::from_secs(5),
        initial_attempts: 1,
        ..ConnectOptions::default()
    };
    let began = std::time::Instant::now();
    run_session(
        target,
        options,
        Arc::clone(&shared),
        fixture.signals,
        Arc::new(|| {}),
    )
    .await;
    // Collect a peer failure without unwinding past the owned watchdog.
    let peer_result = join_owned_peer(peer, Duration::from_secs(3)).await;
    let (entered, before_release) = watchdog.join().unwrap();
    assert!(entered, "actual rustls mTLS requested the held key");
    assert_eq!(
        peer_result.unwrap(),
        1,
        "timeout cancelled the pending upgrade"
    );
    assert!(began.elapsed() >= Duration::from_millis(1200));
    assert!(!before_release.iter().any(|row| matches!(
        row["phase"].as_str().unwrap(),
        "tls_sign_returned" | "dial_deadline" | "initial_deadline"
    )));
    let events = rows(&recorded);
    let started = events
        .iter()
        .position(|row| row["phase"] == "tls_sign_started")
        .unwrap();
    let returned = events
        .iter()
        .position(|row| row["phase"] == "tls_sign_returned")
        .unwrap();
    let deadline = events
        .iter()
        .position(|row| row["phase"] == "dial_deadline")
        .unwrap();
    assert!(started < returned && returned < deadline);
    assert_eq!(
        events[started]["connection"],
        events[returned]["connection"]
    );
    assert_eq!(events[started]["attempt"], events[returned]["attempt"]);
    assert!(events[returned]["elapsed_ms"].as_u64().unwrap() >= 1200);
    assert_eq!(lock(&shared).status(), Status::Failed);
    assert!(
        !events
            .iter()
            .any(|row| matches!(row["phase"].as_str().unwrap(), "hello_sent" | "input_ready"))
    );
    assert!(!serde_json::to_string(&events).unwrap().contains("private-"));
}
