//! phux-wcjrl.2: the producer kit's CLI contract. `phux agent doc serve`
//! publishes each stdin line, prints each viewer intent as a JSON line,
//! and re-adopts its document across a graceful upgrade; `phux agent doc
//! folder` lists a directory and opens a subdirectory on `navigate`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;

use std::io::{BufReader, Write as _};
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

fn snapshot(server: &ServerGuard, doc: &str) -> String {
    let out = server
        .cmd(&["agent", "doc", "snapshot", doc])
        .stderr(Stdio::null())
        .output()
        .expect("doc snapshot");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json_line(reader: &mut impl std::io::BufRead) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("a JSON line");
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("JSON line: {line:?}"))
}

fn started(name: &str) -> ServerGuard {
    let server = ServerGuard::builder(name)
        .seed_command("exec sleep 600")
        .start();
    assert!(
        poll(Duration::from_secs(10), || ok(&server, &["ls"])),
        "the server should accept connections"
    );
    server
}

#[test]
#[ignore = "spawns a real phux server + performs a real in-place re-exec; run via `just e2e`."]
fn serve_publishes_lines_relays_intents_and_readopts_across_upgrade() {
    let server = started("prodkit");
    let mut serve = server
        .cmd(&[
            "agent",
            "doc",
            "serve",
            "--type",
            "text/markdown",
            "--name",
            "kit",
            "--pane",
            SESSION,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("doc serve");
    let mut stdin = serve.stdin.take().unwrap();
    let mut stdout = BufReader::new(serve.stdout.take().unwrap());
    let opened = json_line(&mut stdout);
    assert_eq!(opened["event"], "opened");
    let doc = opened["resource"].as_str().expect("resource").to_owned();

    writeln!(stdin, r#"{{"text": "first"}}"#).unwrap();
    assert!(
        poll(Duration::from_secs(10), || snapshot(&server, &doc)
            == "first\n"),
        "a stdin line replaces the document"
    );
    assert!(ok(&server, &["agent", "page", "navigate", &doc, "next"]));
    let input = json_line(&mut stdout);
    assert_eq!(
        (input["event"].as_str(), input["intent"].as_str()),
        (Some("input"), Some("navigate"))
    );
    assert_eq!(input["url"], "next");

    let pid = server.pid();
    assert!(ok(&server, &["upgrade"]), "`phux upgrade` should ack");
    assert!(
        poll(Duration::from_secs(15), || common::process_exists(pid)
            && ok(&server, &["ls"])),
        "the resumed server should accept connections again"
    );
    writeln!(stdin, r#"{{"base64": "c2Vjb25k"}}"#).unwrap();
    assert!(
        poll(Duration::from_secs(20), || snapshot(&server, &doc)
            == "second\n"),
        "serve redialed and published to the same document"
    );
    assert!(ok(&server, &["agent", "page", "key", &doc, "Enter"]));
    let input = json_line(&mut stdout);
    assert_eq!(
        (input["intent"].as_str(), input["key"].as_str()),
        (Some("key"), Some("Enter"))
    );

    drop(stdin);
    assert!(serve.wait().unwrap().success(), "stdin's end ends serve");
}

#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn folder_lists_a_directory_and_navigates_into_a_subdirectory() {
    let server = started("prodfold");
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("inner")).unwrap();
    std::fs::write(dir.path().join("top.txt"), b"hi").unwrap();
    let mut folder = server
        .cmd(&[
            "agent",
            "doc",
            "folder",
            "--pane",
            SESSION,
            "--interval",
            "1",
        ])
        .arg(dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("doc folder");
    let opened = json_line(&mut BufReader::new(folder.stdout.take().unwrap()));
    let doc = opened["resource"].as_str().expect("resource").to_owned();
    assert!(
        poll(Duration::from_secs(10), || {
            let listing = snapshot(&server, &doc);
            listing.contains("| inner/ |") && listing.contains("| top.txt | 2 B |")
        }),
        "the listing names both entries"
    );
    std::fs::write(dir.path().join("inner/deep.txt"), b"x").unwrap();
    assert!(ok(&server, &["agent", "page", "navigate", &doc, "inner"]));
    assert!(
        poll(Duration::from_secs(10), || snapshot(&server, &doc)
            .contains("| deep.txt |")),
        "navigate opens the subdirectory"
    );
    folder.kill().ok();
    folder.wait().ok();
}
