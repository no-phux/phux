//! A wait for mail is parked in the keeper until mail arrives or its
//! deadline passes, and each message is handed to one waiter once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "assertions in an integration test"
)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use phux_work::{Client, Endpoint, KeeperExit, Mail, OperationId, SendMail, WaitMail, run_keeper};

fn endpoint(root: &Path) -> Endpoint {
    Endpoint::new(&root.join("state"), &root.join("run"))
}

fn keeper(endpoint: &Endpoint, idle: Duration) -> thread::JoinHandle<KeeperExit> {
    let serving = endpoint.clone();
    let handle = thread::spawn(move || run_keeper(&serving, idle).expect("keeper"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound its socket");
        thread::sleep(Duration::from_millis(5));
    }
    handle
}

fn send(client: &Client, to: &[&str], body: &str) {
    client
        .send_mail(SendMail {
            thread: "t".into(),
            from: "pi@749".into(),
            to: to.iter().map(|to| (*to).to_owned()).collect(),
            body: body.into(),
            operation_id: OperationId::generate().unwrap(),
        })
        .unwrap();
}

fn waiting(
    endpoint: &Endpoint,
    reader: &str,
    timeout: Duration,
) -> thread::JoinHandle<(Vec<Mail>, Duration)> {
    let client = Client::new(endpoint.clone(), None);
    let request = WaitMail {
        reader: reader.into(),
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap(),
        limit: 0,
    };
    thread::spawn(move || {
        let started = Instant::now();
        let mail = client.wait_mail(request).expect("wait");
        (mail, started.elapsed())
    })
}

/// Give spawned waiters time to reach the keeper and park.
const PARK: Duration = Duration::from_millis(300);

#[test]
fn a_wait_blocks_until_mail_arrives_then_returns_it() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint(root.path());
    let keeper = keeper(&endpoint, Duration::from_millis(300));
    let waiter = waiting(&endpoint, "@730", Duration::from_secs(30));
    let client = Client::new(endpoint, None);
    thread::sleep(PARK);
    send(&client, &["opencode@730"], "landed");
    let (mail, elapsed) = waiter.join().unwrap();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].body, "landed");
    assert!(elapsed >= PARK, "it waited: {elapsed:?}");
    assert!(elapsed < Duration::from_secs(10), "woken by the send");
    assert_eq!(keeper.join().unwrap(), KeeperExit::Idle);
}

#[test]
fn a_wait_with_no_mail_returns_empty_at_its_deadline_and_holds_the_keeper_up() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint(root.path());
    let keeper = keeper(&endpoint, Duration::from_millis(100));
    let (mail, elapsed) = waiting(&endpoint, "@730", Duration::from_millis(600))
        .join()
        .unwrap();
    assert!(mail.is_empty());
    assert!(elapsed >= Duration::from_millis(600), "{elapsed:?}");
    assert_eq!(keeper.join().unwrap(), KeeperExit::Idle);
}

#[test]
fn concurrent_waiters_for_one_reader_share_each_message_once() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint(root.path());
    let keeper = keeper(&endpoint, Duration::from_millis(300));
    let first = waiting(&endpoint, "@730", Duration::from_millis(1500));
    let second = waiting(&endpoint, "@730", Duration::from_millis(1500));
    let other = waiting(&endpoint, "@731", Duration::from_secs(30));
    let client = Client::new(endpoint, None);
    thread::sleep(PARK);
    send(&client, &["@730", "@731"], "both");
    let mut received: Vec<usize> = [first, second]
        .into_iter()
        .map(|waiter| waiter.join().unwrap().0.len())
        .collect();
    received.sort_unstable();
    assert_eq!(received, [0, 1], "one waiter takes it, the other times out");
    assert_eq!(other.join().unwrap().0.len(), 1, "another reader's waiter");
    let again = client
        .wait_mail(WaitMail {
            reader: "@730".into(),
            timeout_ms: 0,
            limit: 0,
        })
        .unwrap();
    assert!(again.is_empty(), "taken mail is not taken twice");
    assert_eq!(keeper.join().unwrap(), KeeperExit::Idle);
}

#[test]
fn mail_already_waiting_is_returned_at_once() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint(root.path());
    let keeper = keeper(&endpoint, Duration::from_millis(300));
    let client = Client::new(endpoint.clone(), None);
    send(&client, &["@730"], "early");
    let (mail, elapsed) = waiting(&endpoint, "@730", Duration::from_secs(30))
        .join()
        .unwrap();
    assert_eq!(mail.len(), 1);
    assert!(elapsed < Duration::from_secs(10));
    assert_eq!(keeper.join().unwrap(), KeeperExit::Idle);
}
