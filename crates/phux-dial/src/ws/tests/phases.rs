//! Observe the actual socket/TLS/HTTP operations, without altering dial policy.
use super::*;
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;

const SECURE: [WsPhase; 6] = [
    WsPhase::Resolve,
    WsPhase::TcpConnect,
    WsPhase::TlsConfig,
    WsPhase::TlsHandshake,
    WsPhase::Upgrade,
    WsPhase::Upgraded,
];

async fn exchange(secure: bool, observed: bool) -> Vec<WsPhase> {
    let directory = tempfile::tempdir().unwrap();
    let config = crate::testing::server_tls(directory.path(), None);
    let pin = crate::cert::cert_fingerprint(&directory.path().join("cert.pem")).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let stream: ClientStream = if secure {
            Box::new(acceptor.accept(tcp).await.unwrap())
        } else {
            Box::new(tcp)
        };
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Binary(b"round-trip".to_vec().into())
        );
        ws.send(Message::Binary(b"answer".to_vec().into()))
            .await
            .unwrap();
    };
    let phases = Mutex::new(Vec::new());
    let client = async {
        let plan = WsDial {
            url: format!(
                "{}://{address}/private-path",
                if secure { "wss" } else { "ws" }
            ),
            token: Some("abcd".to_owned()),
            trust: CertTrust::Pinned(pin),
            tls_server_name: None,
            identity: Some(TlsClientIdentity::None),
        };
        let mut ws = if observed {
            dial_observed(&plan, |phase| phases.lock().unwrap().push(phase))
                .await
                .unwrap()
        } else {
            dial(&plan).await.unwrap()
        };
        ws.send(Message::Binary(b"round-trip".to_vec().into()))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Binary(b"answer".to_vec().into())
        );
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(server, client);
    })
    .await
    .unwrap();
    phases.into_inner().unwrap()
}

#[tokio::test]
async fn observations_preserve_plain_and_pinned_tls_round_trips() {
    for secure in [false, true] {
        let phases = exchange(secure, true).await;
        let expected = if secure {
            SECURE.to_vec()
        } else {
            vec![
                WsPhase::Resolve,
                WsPhase::TcpConnect,
                WsPhase::Upgrade,
                WsPhase::Upgraded,
            ]
        };
        assert_eq!(phases, expected);
        assert!(
            exchange(secure, false).await.is_empty(),
            "the original no-op entry stays equivalent"
        );
    }
}

#[tokio::test]
async fn malformed_dns_and_tcp_failures_leave_only_the_entered_boundary() {
    let phases = Mutex::new(Vec::new());
    let observe = |phase| phases.lock().unwrap().push(phase);
    assert!(
        dial_observed(&plain_dial("https://127.0.0.1".to_owned()), observe)
            .await
            .is_err()
    );
    assert!(phases.lock().unwrap().is_empty());
    let unresolved = tokio::time::timeout(
        Duration::from_secs(5),
        dial_observed(
            &plain_dial("ws://phux-test-nxdomain.invalid:1".to_owned()),
            observe,
        ),
    )
    .await
    .expect("caller bounds resolver observation; OS resolver work may outlive cancellation");
    assert!(matches!(unresolved, Err(DialError::Unreachable(_))));
    assert_eq!(*phases.lock().unwrap(), [WsPhase::Resolve]);
    phases.lock().unwrap().clear();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    assert!(matches!(
        dial_observed(&plain_dial(format!("ws://{address}")), observe).await,
        Err(DialError::Unreachable(_))
    ));
    assert_eq!(*phases.lock().unwrap(), SECURE[..2]);
}

#[tokio::test]
async fn tls_configuration_pin_and_http_refusals_do_not_invent_completion() {
    for last in [WsPhase::TlsConfig, WsPhase::TlsHandshake, WsPhase::Upgrade] {
        let directory = tempfile::tempdir().unwrap();
        let tls = crate::testing::server_tls(directory.path(), None);
        let pin = crate::cert::cert_fingerprint(&directory.path().join("cert.pem")).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            if let Ok(mut tls) = acceptor.accept(tcp).await {
                use tokio::io::AsyncReadExt;
                let mut request = [0; 4096];
                assert!(tls.read(&mut request).await.unwrap() > 0);
                tls.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap();
            }
        };
        let mut plan = plain_dial(format!("wss://{address}"));
        plan.identity = Some(TlsClientIdentity::None);
        plan.trust = CertTrust::Pinned(pin);
        if last == WsPhase::TlsConfig {
            plan.identity = Some(TlsClientIdentity::PemFiles {
                certificate: directory.path().join("missing-cert"),
                private_key: directory.path().join("missing-key"),
            });
        }
        if last == WsPhase::TlsHandshake {
            plan.trust = CertTrust::Pinned("00".repeat(32));
        }
        let phases = Mutex::new(Vec::new());
        let client = dial_observed(&plan, |phase| phases.lock().unwrap().push(phase));
        let ((), result) = tokio::time::timeout(
            Duration::from_secs(5),
            Box::pin(async { tokio::join!(server, client) }),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(DialError::Connect(_))), "{result:?}");
        let expected = SECURE.iter().position(|phase| *phase == last).unwrap();
        assert_eq!(*phases.lock().unwrap(), SECURE[..=expected]);
    }
}
