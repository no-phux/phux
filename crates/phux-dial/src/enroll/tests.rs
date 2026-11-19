use super::*;
use std::sync::Mutex;

async fn exchange(reply: Vec<u8>, observed: bool) -> (Result<String, DialError>, Vec<EnrollPhase>) {
    let directory = tempfile::tempdir().unwrap();
    let endpoint = crate::testing::quic_server(directory.path(), ENROLL_ALPN);
    let dial = EnrollDial {
        addr: endpoint.local_addr().unwrap(),
        server_name: "localhost".to_owned(),
        trust: CertTrust::SkipVerify,
        inner: None,
    };
    let request = Request {
        ticket: vec![0xab; 32],
        csr: vec![1, 2, 3],
    };
    let encoded = request.encode().unwrap();
    let server = tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        assert_eq!(
            recv.read_to_end(phux_protocol::enroll::MAX_REQUEST)
                .await
                .unwrap(),
            encoded
        );
        send.write_all(&reply).await.unwrap();
        send.finish().unwrap();
        let _ = send.stopped().await;
        connection.closed().await;
    });
    let phases = Mutex::new(Vec::new());
    let result = if observed {
        enroll_observed(&dial, &request, |phase| phases.lock().unwrap().push(phase)).await
    } else {
        enroll(&dial, &request).await
    };
    server.await.unwrap();
    (result, phases.into_inner().unwrap())
}

#[tokio::test]
async fn observed_direct_exchange_preserves_the_noop_wrapper_and_operation_order() {
    let reply = Reply::Issued("public-chain-sentinel".to_owned()).encode();
    let (observed, phases) = exchange(reply.clone(), true).await;
    let (plain, none) = exchange(reply, false).await;
    assert_eq!(observed.unwrap(), plain.unwrap());
    assert!(none.is_empty());
    assert_eq!(
        phases,
        [
            EnrollPhase::Prepare,
            EnrollPhase::Connect,
            EnrollPhase::OpenStream,
            EnrollPhase::Write,
            EnrollPhase::Read,
            EnrollPhase::Drain,
            EnrollPhase::Decode,
        ]
    );
}

#[tokio::test]
async fn refused_and_malformed_replies_preserve_the_last_decode_boundary() {
    let (refused, phases) = exchange(Reply::Refused.encode(), true).await;
    assert!(matches!(refused, Err(DialError::AuthRefused(_))));
    assert_eq!(phases.last(), Some(&EnrollPhase::Decode));
    let (malformed, phases) = exchange(vec![0xff], true).await;
    assert!(matches!(malformed, Err(DialError::Connect(_))));
    assert_eq!(phases.last(), Some(&EnrollPhase::Decode));
}

#[tokio::test]
async fn invalid_request_never_reaches_the_network() {
    let phases = Mutex::new(Vec::new());
    let dial = EnrollDial {
        addr: "127.0.0.1:1".parse().unwrap(),
        server_name: "private-host-sentinel".to_owned(),
        trust: CertTrust::SkipVerify,
        inner: None,
    };
    let result = enroll_observed(
        &dial,
        &Request {
            ticket: vec![],
            csr: vec![],
        },
        |phase| phases.lock().unwrap().push(phase),
    )
    .await;
    assert!(matches!(result, Err(DialError::Connect(_))));
    assert_eq!(phases.into_inner().unwrap(), [EnrollPhase::Prepare]);
}
