//! A satellite's mail and claims live in its hub's store, reached through a
//! [`Gateway`] that puts everything the satellite speaks as under its name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "assertions in an integration test"
)]

use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use phux_work::federation::Gateway;
use phux_work::{
    ClaimFilter, ClaimOutcome, Client, Endpoint, Error, Exchange, ListFilter, OperationId,
    ReadMail, SendMail, TakeClaim, WaitMail, run_keeper,
};

/// The satellite's link to its hub, in process.
#[derive(Debug)]
struct Link(Gateway);

impl Exchange for Link {
    fn exchange(
        &self,
        line: &str,
        _reply: Duration,
        lifeline: Option<BorrowedFd<'_>>,
    ) -> Result<String, Error> {
        Ok(self.0.answer(line, lifeline))
    }
}

struct Federation {
    _root: tempfile::TempDir,
    hub: Client,
    endpoint: Endpoint,
}

impl Federation {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(&root.path().join("state"), &root.path().join("run"));
        keeper(&endpoint);
        Self {
            hub: Client::new(endpoint.clone(), None),
            endpoint,
            _root: root,
        }
    }

    fn satellite(&self, name: &str) -> Client {
        let gateway = Gateway::new(Client::new(self.endpoint.clone(), None), name).unwrap();
        Client::remote(Arc::new(Link(gateway)))
    }
}

fn keeper(endpoint: &Endpoint) {
    let serving = endpoint.clone();
    thread::spawn(move || run_keeper(&serving, Duration::from_secs(30)).expect("keeper"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound its socket");
        thread::sleep(Duration::from_millis(5));
    }
}

fn send(client: &Client, from: &str, to: &[&str], body: &str) -> Result<(), Error> {
    client
        .send_mail(SendMail {
            thread: "t".into(),
            from: from.into(),
            to: to.iter().map(|to| (*to).to_owned()).collect(),
            body: body.into(),
            operation_id: OperationId::generate().unwrap(),
        })
        .map(drop)
}

fn read(client: &Client, reader: &str) -> Result<Vec<phux_work::Mail>, Error> {
    client.read_mail(ReadMail {
        reader: reader.into(),
        thread: None,
        all: true,
        mark_read: false,
        limit: 0,
    })
}

fn take(client: &Client, holder: &str, path: &str) -> ClaimOutcome {
    client
        .take_claim(TakeClaim {
            holder: holder.into(),
            paths: vec![path.into()],
            exclusive: true,
            ttl_secs: 60,
            note: String::new(),
        })
        .unwrap()
}

fn holders(client: &Client) -> Vec<String> {
    let mut holders: Vec<_> = client
        .claims(ClaimFilter::default())
        .unwrap()
        .into_iter()
        .map(|claim| claim.holder)
        .collect();
    holders.sort();
    holders
}

#[test]
fn each_side_reads_the_other_by_its_own_addresses() {
    let net = Federation::new();
    let boxed = net.satellite("min-api");

    send(&boxed, "pi@3", &["hub/@749"], "tests pass in the box").unwrap();
    let mail = read(&net.hub, "@749").unwrap();
    assert_eq!(mail.len(), 1);
    assert_eq!(mail[0].from, "min-api/pi@3");
    assert_eq!(mail[0].to, ["@749"]);

    send(&net.hub, "claude@749", &["min-api/@3"], "merge it").unwrap();
    let mail = read(&boxed, "@3").unwrap();
    let bodies: Vec<_> = mail.iter().map(|mail| mail.body.as_str()).collect();
    assert_eq!(bodies, ["merge it"]);
    assert_eq!(mail[0].from, "hub/claude@749");
    assert_eq!(mail[0].to, ["@3"]);
}

#[test]
fn a_satellite_cannot_speak_as_the_hub_or_another_satellite() {
    let net = Federation::new();
    let boxed = net.satellite("min-api");
    send(&net.hub, "phall", &["@749"], "hub only").unwrap();
    for other in ["hub/@749", "min-web/@2"] {
        assert!(
            matches!(read(&boxed, other), Err(Error::InvalidText { .. })),
            "{other}"
        );
        assert!(send(&boxed, other, &["@749"], "forged").is_err(), "{other}");
    }
    assert_eq!(read(&net.hub, "@749").unwrap().len(), 1, "nothing forged");
}

#[test]
fn satellites_reach_each_other_through_the_hub() {
    let net = Federation::new();
    send(&net.satellite("min-api"), "@3", &["min-web/@2"], "rebased").unwrap();
    let mail = read(&net.satellite("min-web"), "@2").unwrap();
    assert_eq!(mail[0].from, "min-api/@3");
    assert_eq!(mail[0].to, ["@2"]);
}

#[test]
fn claims_are_shared_and_held_under_the_satellite_name() {
    let net = Federation::new();
    let boxed = net.satellite("min-api");
    assert!(matches!(
        take(&boxed, "@3", "/repo/src"),
        ClaimOutcome::Granted(_)
    ));
    let ClaimOutcome::Conflict(held) = take(&net.hub, "@749", "/repo") else {
        panic!("the box's claim blocks the hub");
    };
    assert_eq!(held[0].holder, "min-api/@3");
    assert!(matches!(
        take(&net.hub, "@749", "/other"),
        ClaimOutcome::Granted(_)
    ));
    assert_eq!(holders(&boxed), ["@3", "hub/@749"]);
    assert_eq!(holders(&net.hub), ["@749", "min-api/@3"]);
}

#[test]
fn a_wait_through_the_gateway_takes_hub_mail() {
    let net = Federation::new();
    let boxed = net.satellite("min-api");
    let waiter = thread::spawn(move || {
        boxed
            .wait_mail(WaitMail {
                reader: "@3".into(),
                timeout_ms: 30_000,
                limit: 0,
            })
            .unwrap()
    });
    thread::sleep(Duration::from_millis(300));
    send(&net.hub, "@749", &["min-api/@3"], "wake up").unwrap();
    let mail = waiter.join().unwrap();
    assert_eq!(mail[0].body, "wake up");
    assert_eq!(mail[0].from, "hub/@749");
}

#[test]
fn only_mail_and_claims_cross() {
    let net = Federation::new();
    let refused = net.satellite("min-api").list(ListFilter {
        repo: None,
        archived: false,
    });
    assert!(
        matches!(refused, Err(Error::InvalidText { .. })),
        "{refused:?}"
    );
    assert!(Gateway::new(net.hub.clone(), "hub").is_err());
    assert!(Gateway::new(net.hub, "a b").is_err());
}
