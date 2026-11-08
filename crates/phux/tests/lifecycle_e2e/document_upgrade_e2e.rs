//! phux-g12pw: documents survive a graceful server upgrade. A typed
//! document keeps its id and newest document, and a running surface mirror
//! redials and re-adopts its surface, so viewers keep the same resource.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::ServerGuard;

const SESSION: &str = "work";

fn poll<F: FnMut() -> bool>(deadline: Duration, mut f: F) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn ok(server: &ServerGuard, args: &[&str]) -> bool {
    server
        .cmd(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn producer(server: &ServerGuard, resource: &str) -> Option<String> {
    let out = server
        .cmd(&["resource", "show", resource, "--json"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json["producer"].as_str().map(str::to_owned)
}

fn upgrade(server: &ServerGuard) {
    let pid = server.pid();
    assert!(ok(server, &["upgrade"]), "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(15), || common::process_exists(pid)
            && ok(server, &["ls"])),
        "the resumed server should accept connections again"
    );
}

#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn a_document_and_a_running_mirror_survive_graceful_upgrade() {
    let server = ServerGuard::builder("docupg")
        .seed_command("exec sleep 600")
        .start();
    assert!(
        poll(Duration::from_secs(10), || ok(&server, &["ls"])),
        "the server should accept connections"
    );

    // A typed document, published once by a producer that then exits.
    let open = server
        .cmd(&["agent", "doc", "open", SESSION, "--type", "text/markdown"])
        .output()
        .expect("doc open");
    let doc = String::from_utf8_lossy(&open.stdout).trim().to_owned();
    assert!(doc.starts_with('@'), "open prints the document id: {doc:?}");
    let mut publish = server
        .cmd(&["agent", "doc", "publish", &doc])
        .stdin(Stdio::piped())
        .spawn()
        .expect("doc publish");
    publish
        .stdin
        .take()
        .unwrap()
        .write_all(b"# before the upgrade")
        .unwrap();
    assert!(publish.wait().unwrap().success(), "the publish lands");

    // A running surface mirror; a dry run publishes a blank frame when the
    // capture is refused, so no Screen Recording grant or compositor is
    // needed.
    let mut mirror = server
        .cmd(&[
            "agent",
            "surface",
            "mirror",
            "display",
            "--pane",
            SESSION,
            "--dry-run-input",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("surface mirror");
    let mut line = String::new();
    BufReader::new(mirror.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("the mirror names its surface");
    let line: serde_json::Value = serde_json::from_str(&line).expect("JSON line");
    let surface = line["resource"].as_str().expect("resource").to_owned();
    assert!(
        poll(Duration::from_secs(10), || producer(&server, &surface)
            .as_deref()
            == Some("connected")),
        "the mirror produces its surface"
    );

    upgrade(&server);

    let snapshot = server
        .cmd(&["agent", "doc", "snapshot", &doc])
        .output()
        .expect("doc snapshot");
    assert_eq!(
        String::from_utf8_lossy(&snapshot.stdout),
        "# before the upgrade",
        "the document crossed with its id and newest document"
    );
    assert!(
        poll(Duration::from_secs(20), || producer(&server, &surface)
            .as_deref()
            == Some("connected")),
        "the mirror redialed and re-adopted the same surface"
    );
    assert!(
        mirror.try_wait().unwrap().is_none(),
        "the mirror is still running"
    );
    mirror.kill().ok();
    mirror.wait().ok();
}
