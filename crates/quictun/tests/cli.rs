//! The `quictun` binary end to end, as an operator runs it: `pair`, then
//! `relay`, `expose` in front of a local TCP service, and `connect`; a plain
//! TCP client goes through all of it, half-closing before it reads.

#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(20);

fn quictun(state: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_quictun"));
    command
        .env("RUST_LOG", "warn")
        .args(args)
        .arg("--state")
        .arg(state)
        .kill_on_drop(true);
    command
}

/// Spawn with stdout piped and return the value printed after `label`.
async fn spawn_reading(
    mut command: Command,
    label: &str,
) -> (Child, String, BufReader<ChildStdout>) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn quictun");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout"));
    let value = read_value(&mut lines, label).await;
    (child, value, lines)
}

/// The first value printed after `label` (`label: value`).
async fn read_value(lines: &mut BufReader<ChildStdout>, label: &str) -> String {
    loop {
        let mut line = String::new();
        assert_ne!(
            lines.read_line(&mut line).await.expect("read"),
            0,
            "no {label}"
        );
        if let Some(value) = line
            .strip_prefix(label)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return value.split_whitespace().next().expect("value").to_owned();
        }
    }
}

/// A local TCP service: read each connection to its end, then say how much.
async fn counting_target() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let _ = stream.read_to_end(&mut request).await;
                let reply = format!("got {} bytes", request.len());
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

/// One request through `connect`: write, half-close, read the reply.
async fn request(through: &str, body: &[u8]) -> Option<Vec<u8>> {
    let mut stream = tokio::net::TcpStream::connect(through).await.ok()?;
    stream.write_all(body).await.ok()?;
    stream.shutdown().await.ok()?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.ok()?;
    (!reply.is_empty()).then_some(reply)
}

#[tokio::test]
async fn pair_relay_expose_and_connect_carry_tcp_end_to_end() {
    timeout(DEADLINE, async {
        let relay_state = tempfile::tempdir().expect("relay state");
        let service_state = tempfile::tempdir().expect("service state");

        let pair = quictun(relay_state.path(), &["pair", "--route", "laptop"]);
        let (_pair, token, mut pair_out) = spawn_reading(pair, "token").await;
        let relay_pin = read_value(&mut pair_out, "relay pin").await;
        let token_file = service_state.path().join("token");
        std::fs::write(&token_file, &token).expect("token file");
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");

        let relay = quictun(relay_state.path(), &["relay", "--listen", "127.0.0.1:0"]);
        let (_relay, relay_addr, mut relay_out) = spawn_reading(relay, "relay").await;
        assert_eq!(read_value(&mut relay_out, "relay pin").await, relay_pin);

        let target = counting_target().await.to_string();
        let token_path = token_file.to_str().expect("utf-8 path");
        let expose = quictun(
            service_state.path(),
            &[
                "expose",
                "--relay",
                &relay_addr,
                "--relay-pin",
                &relay_pin,
                "--to",
                &target,
                "--token-file",
                token_path,
            ],
        );
        let (_expose, service_pin, _expose_out) = spawn_reading(expose, "service pin").await;

        let connect = quictun(
            service_state.path(),
            &[
                "connect",
                "--relay",
                &relay_addr,
                "--relay-pin",
                &relay_pin,
                "--route",
                "laptop",
                "--service-pin",
                &service_pin,
                "--listen",
                "127.0.0.1:0",
            ],
        );
        let (_connect, listening, _connect_out) = spawn_reading(connect, "listening").await;

        // The tunnel comes up asynchronously; retry until it carries.
        let reply = loop {
            if let Some(reply) = request(&listening, b"hello through the relay").await {
                break reply;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(reply, b"got 23 bytes");
    })
    .await
    .expect("deadline");
}

#[tokio::test]
async fn bad_invocations_fail_and_name_the_problem() {
    let state = tempfile::tempdir().expect("state");
    for (args, needle) in [
        (&["relay"][..], "--listen is required"),
        (&["pair", "--route", "Bad_Name"][..], "invalid route name"),
        (&["frobnicate"][..], "unknown command"),
        (
            &[
                "connect",
                "--relay",
                "127.0.0.1:1",
                "--relay-pin",
                "x",
                "--route",
                "r",
                "--listen",
                "127.0.0.1:0",
            ][..],
            "--service-pin",
        ),
    ] {
        let output = quictun(state.path(), args).output().await.expect("run");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
}
