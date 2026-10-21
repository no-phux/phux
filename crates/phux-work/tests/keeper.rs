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
    AddObjective, Client, Endpoint, Error, KeeperExit, ListFilter, ObjectiveState, OperationId,
    StateEdit, run_keeper,
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
            b"{\"build\":\"0.0.0-other\",\"request\":{\"op\":\"unknown_future_operation\",\"shape\":[1,2,3]}}\n",
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
fn fenced_leases_cross_the_keeper_without_losing_mutation_preconditions() {
    use phux_work::{ClaimFilter, ClaimOutcome, ReleaseClaim, RenewClaim, TakeClaim};
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let keeper = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, Duration::from_millis(500)))
    };
    wait_for_socket(&endpoint);
    let client = Client::new(endpoint, None);
    let request = TakeClaim {
        holder: "pi@749".into(),
        paths: vec!["/r/a".into(), "/r/b".into()],
        exclusive: true,
        ttl_secs: 600,
        note: "keeper regression".into(),
    };
    let ClaimOutcome::Granted(first) = client.take_claim(request.clone()).expect("take") else {
        panic!("free")
    };
    let fence = first[0].fence;
    assert_eq!(first[1].fence, fence);
    assert!(matches!(
        client.take_claim(request.clone()).expect("repeat"),
        ClaimOutcome::Conflict(_)
    ));
    let renewed = client
        .renew_claim(RenewClaim {
            holder: "@749".into(),
            fence,
            ttl_secs: 1200,
        })
        .expect("renew");
    assert_eq!(renewed.len(), 2);
    let missing = client.renew_claim(RenewClaim {
        holder: "@730".into(),
        fence,
        ttl_secs: 1200,
    });
    assert!(matches!(missing, Err(Error::NoClaim { fence: other }) if other == fence));
    let cleanup = ReleaseClaim {
        holder: "@749".into(),
        fence,
    };
    assert_eq!(client.release_claim(cleanup.clone()).expect("release"), 2);
    let ClaimOutcome::Granted(next) = client.take_claim(request).expect("reacquire") else {
        panic!("released")
    };
    assert!(next[0].fence > fence);
    assert_eq!(client.release_claim(cleanup).expect("stale cleanup"), 0);
    assert_eq!(client.claims(ClaimFilter::default()).expect("live"), next);
    keeper.join().expect("join").expect("keeper");
}

#[test]
fn the_keeper_stays_up_while_work_is_open_and_exits_once_it_is_not() {
    let root = tempfile::tempdir().expect("tempdir");
    let endpoint = endpoint(root.path());
    let idle = Duration::from_millis(300);
    let keeper = {
        let endpoint = endpoint.clone();
        thread::spawn(move || run_keeper(&endpoint, idle))
    };
    wait_for_socket(&endpoint);
    let client = Client::new(endpoint.clone(), None);
    let added = client.add(add("phux", "open")).expect("add");
    client
        .record_state(StateEdit {
            alias: added.objective.alias.clone(),
            state: ObjectiveState::Working,
            expected_revision: added.objective.revision,
            operation_id: OperationId::generate().expect("id"),
            actor: "tester".to_owned(),
            event_kind: None,
        })
        .expect("working");
    thread::sleep(idle + Duration::from_millis(200));
    assert!(
        endpoint.socket.exists(),
        "a working task keeps the keeper up"
    );
    assert_eq!(
        client
            .show(added.objective.alias.clone())
            .expect("show")
            .state,
        ObjectiveState::Working
    );
    let shown = client.show(added.objective.alias).expect("show");
    client
        .record_state(StateEdit {
            alias: shown.alias,
            state: ObjectiveState::DoneUnchecked,
            expected_revision: shown.revision,
            operation_id: OperationId::generate().expect("id"),
            actor: "tester".to_owned(),
            event_kind: Some("done_unchecked".to_owned()),
        })
        .expect("done");
    assert_eq!(
        keeper.join().expect("join").expect("keeper"),
        KeeperExit::Idle
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
