//! `phux forward -L ... -L ...` against a real server.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::Stdio;
use std::time::Duration;

use crate::common;

/// A target on the server's loopback that greets every connection with `name`.
fn target(name: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("target");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let _ = (&stream).write_all(name.as_bytes());
        }
    });
    port
}

/// Every `-L` is served, not only the last, with nothing but the two lines
/// that say so.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn every_forward_is_served() {
    let server = common::ServerGuard::builder("fwd").start();
    let specs = [target("first"), target("second")].map(|port| format!("0:127.0.0.1:{port}"));
    let mut child = common::ServerProcess::from_child(
        server
            .cmd(&["forward", "-L", &specs[0], "-L", &specs[1]])
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn forward"),
        std::path::PathBuf::new(),
    );
    let stdout = child.child_mut().stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    for greeting in ["first", "second"] {
        let line = lines.next().expect("a forward").expect("read");
        let here = line.split_whitespace().nth(1).expect("the local address");
        let mut stream = TcpStream::connect(here).unwrap_or_else(|err| panic!("{line}: {err}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let mut got = String::new();
        stream.read_to_string(&mut got).expect("forwarded bytes");
        assert_eq!(got, greeting, "{line}");
    }
    let _ = child.child_mut().kill();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.child_mut().stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(!stderr.contains("INFO"), "{stderr}");
}
