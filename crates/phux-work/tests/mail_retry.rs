//! A real keeper commits mail, but its first reply never reaches the client.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use phux_work::{Client, Endpoint, Error, KeeperExit, OperationId, ReadMail, SendMail, run_keeper};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::{Duration, Instant};

fn owned_keeper(endpoint: &Endpoint) -> thread::JoinHandle<KeeperExit> {
    let serving = endpoint.clone();
    let keeper = thread::spawn(move || run_keeper(&serving, Duration::from_secs(2)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "owned keeper did not start");
        thread::sleep(Duration::from_millis(5));
    }
    keeper
}

#[test]
fn mail_recovers_same_durable_operation_after_a_real_keeper_reply_is_lost() {
    let root = tempfile::Builder::new()
        .prefix("phux-mail-retry-")
        .tempdir_in("/tmp")
        .unwrap();
    let endpoint = Endpoint::new(&root.path().join("state"), &root.path().join("run"));
    let initial_keeper = owned_keeper(&endpoint);
    let proxy_socket = root.path().join("drop.sock");
    let listener = UnixListener::bind(&proxy_socket).unwrap();
    let actual = endpoint.socket.clone();
    let proxy = thread::spawn(move || {
        let (mut caller, _) = listener.accept().unwrap();
        caller
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = String::new();
        BufReader::new(&mut caller).read_line(&mut request).unwrap();
        let mut backend = UnixStream::connect(actual).unwrap();
        backend
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        backend.write_all(request.as_bytes()).unwrap();
        let mut committed = String::new();
        BufReader::new(&backend).read_line(&mut committed).unwrap();
        assert!(!committed.is_empty(), "real keeper completed the exchange");
        // The store committed and replied to the proxy. Drop that reply and
        // close the caller side: its write is uncertain, not rejected.
        drop(caller);
    });
    let operation = OperationId::generate().unwrap();
    let request = |body: &str| SendMail {
        operation_id: operation,
        thread: "retry".into(),
        from: "sender".into(),
        to: vec!["a".into(), "b".into()],
        body: body.into(),
    };
    let via_proxy = Client::new(
        Endpoint {
            store: endpoint.store.clone(),
            socket: proxy_socket,
        },
        None,
    );
    let lost = via_proxy.send_mail(request("one message")).unwrap_err();
    assert!(matches!(lost, Error::Keeper(_)));
    assert!(lost.write_outcome_unknown());
    proxy.join().unwrap();
    assert_eq!(initial_keeper.join().unwrap(), KeeperExit::Idle);
    let restarted_keeper = owned_keeper(&endpoint);
    // A fresh caller/adapter uses the original durable identity, not the
    // failed invocation's local observation receipt.
    let restarted = Client::new(endpoint, None);
    let recovered = restarted.send_mail(request("one message")).unwrap();
    assert!(recovered.idempotent_replay);
    assert!(matches!(
        restarted.send_mail(request("different message")),
        Err(Error::OperationConflict { .. })
    ));
    for reader in ["a", "b"] {
        let mail = restarted
            .read_mail(ReadMail {
                reader: reader.into(),
                thread: None,
                all: true,
                mark_read: false,
                limit: 10,
            })
            .unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].id, recovered.mail.id);
    }
    assert_eq!(restarted_keeper.join().unwrap(), KeeperExit::Idle);
}
