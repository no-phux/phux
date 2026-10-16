//! The keeper is the store's only writer, and every caller goes through it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in an integration test"
)]

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use phux_work::{
    AddObjective, Client, Endpoint, Error, KeeperExit, ListFilter, OperationId, run_keeper,
};

fn endpoint(root: &Path) -> Endpoint {
    Endpoint::new(&root.join("state"), &root.join("run"))
}

fn add(repo: &str, title: &str) -> AddObjective {
    AddObjective {
        repo: repo.to_owned(),
        title: title.to_owned(),
        description: String::new(),
        operation_id: OperationId::generate().expect("id"),
        actor: "tester".to_owned(),
    }
}

fn wait_for_socket(endpoint: &Endpoint) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound its socket");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn concurrent_callers_share_one_keeper_and_lose_nothing() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let keeper = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, Duration::from_millis(500)))
    };
    wait_for_socket(&endpoint);

    let callers: Vec<_> = (0..16)
        .map(|index| {
            let client = Client::new(endpoint.clone(), None);
            thread::spawn(move || client.add(add("phux", &format!("task-{index}"))))
        })
        .collect();
    for caller in callers {
        caller.join().expect("join").expect("add");
    }
    let client = Client::new(endpoint.clone(), None);
    let tasks = client
        .list(ListFilter {
            repo: Some("phux".to_owned()),
            archived: false,
        })
        .expect("list");
    let numbers: BTreeSet<_> = tasks.iter().map(|task| task.alias.number).collect();
    assert_eq!(numbers, (1..=16).collect());

    assert_eq!(
        keeper.join().expect("join").expect("keeper"),
        KeeperExit::Idle
    );
    assert!(
        !endpoint.socket.exists(),
        "an idle keeper removes its socket"
    );
}

#[test]
fn replaying_an_operation_through_the_keeper_returns_the_first_result() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let keeper = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, Duration::from_millis(300)))
    };
    wait_for_socket(&endpoint);
    let client = Client::new(endpoint, None);
    let request = add("phux", "once");
    let first = client.add(request.clone()).expect("first");
    let again = client.add(request).expect("replay");
    assert!(!first.idempotent_replay);
    assert!(again.idempotent_replay);
    assert_eq!(first.objective.id, again.objective.id);
    let missing = client.show(phux_work::Alias::parse("phux/9").expect("alias"));
    assert!(
        matches!(missing, Err(Error::NotFound { .. })),
        "{missing:?}"
    );
    keeper.join().expect("join").expect("keeper");
}

#[test]
fn a_second_keeper_for_the_same_store_steps_aside() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let first = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, Duration::from_millis(500)))
    };
    wait_for_socket(&endpoint);
    let second = run_keeper(&endpoint, Duration::from_secs(30)).expect("second");
    assert_eq!(second, KeeperExit::AlreadyRunning);
    assert!(
        endpoint.socket.exists(),
        "the serving keeper keeps its socket"
    );
    assert_eq!(
        first.join().expect("join").expect("first"),
        KeeperExit::Idle
    );
}

#[test]
fn a_request_from_another_build_retires_the_keeper() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let keeper = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, Duration::from_secs(30)))
    };
    wait_for_socket(&endpoint);
    let mut stream = UnixStream::connect(&endpoint.socket).expect("connect");
    stream
        .write_all(
            b"{\"build\":\"0.0.0-other\",\"request\":{\"op\":\"list\",\"repo\":null,\"archived\":false}}\n",
        )
        .expect("write");
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).expect("read");
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("json");
    assert_eq!(reply["result"], "fault");
    assert_eq!(reply["value"]["kind"], "stale");
    assert_eq!(
        keeper.join().expect("join").expect("keeper"),
        KeeperExit::Stale
    );
}

#[test]
fn without_a_launcher_a_missing_keeper_is_an_error() {
    let root = tempfile::tempdir().expect("tempdir");
    let client = Client::new(endpoint(root.path()), None);
    let result = client.list(ListFilter {
        repo: None,
        archived: false,
    });
    assert!(matches!(result, Err(Error::Keeper(_))), "{result:?}");
}
