//! Real CLI processes, private server, inert fake `op`: no vault access.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "isolated e2e assertions"
)]

mod common;
#[path = "common/runner.rs"]
mod runner;

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn eventually<T>(mut condition: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(Instant::now() < deadline, "credential e2e deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn pending(server: &common::ServerGuard) -> String {
    eventually(|| {
        let records: serde_json::Value =
            serde_json::from_str(&server.success(&["approvals", "--json"])).unwrap();
        records["approvals"]
            .as_array()
            .unwrap()
            .first()
            .map(|record| {
                assert_eq!(
                    record["credential"]["request"]["references"][0],
                    "op://dev/test/token"
                );
                record["id"].as_str().unwrap().to_owned()
            })
    })
}

fn consumer(server: &common::ServerGuard) -> ChildGuard {
    ChildGuard(
        server
            .cmd_global(&[
                "credentials",
                "run",
                "--provider",
                "fixture",
                "--env",
                "TEST_API_KEY=op://dev/test/token",
                "--purpose",
                "inert credential e2e consumer",
                "--",
                "/bin/sh",
                "-c",
                "test \"$TEST_API_KEY\" = synthetic-e2e-canary || exit 9; exit 37",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}

fn review(server: &common::ServerGuard, id: &str, answer: &[u8]) -> String {
    let mut cli = common::PtyAttach::start_client(
        "approve",
        &[id, "--socket", server.socket.to_str().unwrap()],
        (120, 30),
        |_, _| {},
    );
    let prompt = eventually(|| {
        let painted = cli.painted();
        painted.contains("[y/N]").then_some(painted)
    });
    for detail in [
        id,
        "op://dev/test/token",
        "Provider label (not identity): \"fixture\"",
        "Bound provider: client ",
        "Purpose (untrusted requester text): \"inert credential e2e consumer\"",
        "unlock 1Password locally",
    ] {
        assert!(
            prompt.contains(detail),
            "missing review detail {detail}: {prompt}"
        );
    }
    cli.send(answer);
    eventually(|| (!cli.is_running()).then_some(()));
    cli.painted()
}

fn assert_unattended_approval_refused(
    server: &common::ServerGuard,
    id: &str,
    consumer: &mut ChildGuard,
    root: &std::path::Path,
) {
    for json in [false, true] {
        let mut args = vec!["approve", id, "--yes"];
        if json {
            args.push("--json");
        }
        let refused = server.cmd_global(&args).output().unwrap();
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        assert_eq!(pending(server), id, "opaque approval must stay pending");
        assert!(!root.join("calls").exists(), "--yes cannot invoke op");
        assert!(consumer.0.try_wait().unwrap().is_none());
    }
    let noninteractive = server
        .cmd_global(&["approve", id, "--json"])
        .output()
        .unwrap();
    assert_eq!(noninteractive.status.code(), Some(2));
    assert!(noninteractive.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&noninteractive.stderr).unwrap();
    assert_eq!(error["error"]["code"], "confirmation_required");
    assert!(String::from_utf8_lossy(&noninteractive.stderr).contains("without --yes"));
}

fn assert_no_secret_output(child: &mut ChildGuard) {
    let mut output = String::new();
    if let Some(mut stdout) = child.0.stdout.take() {
        std::io::Read::read_to_string(&mut stdout, &mut output).unwrap();
    }
    if let Some(mut stderr) = child.0.stderr.take() {
        std::io::Read::read_to_string(&mut stderr, &mut output).unwrap();
    }
    assert!(
        !output.contains("synthetic-e2e-canary"),
        "credential value leaked into CLI output"
    );
}

#[test]
#[ignore = "real CLI on a private server with a fake op; runs in the e2e lane"]
fn approved_injection_survives_another_requesters_cancellation_and_denial_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("op");
    std::fs::write(&fake, r#"#!/bin/sh
set -eu
[ "$1" = read ] && [ "$2" = --no-newline ] && [ "$3" = -- ] && [ "$4" = op://dev/test/token ] || exit 1
[ "$OP_BIOMETRIC_UNLOCK_ENABLED" = true ] || exit 1
[ -z "${OP_SERVICE_ACCOUNT_TOKEN+x}" ] || exit 1
printf x >> "$CREDENTIAL_FIXTURE_ROOT/calls"
while [ ! -f "$CREDENTIAL_FIXTURE_ROOT/release" ]; do sleep 0.02; done
printf synthetic-e2e-canary
"#).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let server = common::ServerGuard::builder("credential-e2e")
        .seed_command("cat")
        .start();
    let path = std::env::join_paths(std::iter::once(dir.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let mut provider = ChildGuard(
        server
            .cmd_global(&["credentials", "serve", "--name", "fixture"])
            .env("PATH", path)
            .env("CREDENTIAL_FIXTURE_ROOT", dir.path())
            .env("OP_SERVICE_ACCOUNT_TOKEN", "inert-must-not-be-inherited")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = provider.0.stderr.take().unwrap();
    let (ready, registered) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead as _;
        let mut leaked = false;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            leaked |= line.contains("synthetic-e2e-canary");
            if line.contains("waiting for approved 1Password requests") {
                let _ = ready.send(());
            }
        }
        leaked
    });
    registered.recv_timeout(Duration::from_secs(20)).unwrap();
    let mut canceled = consumer(&server);
    let first = pending(&server);
    assert_unattended_approval_refused(&server, &first, &mut canceled, dir.path());
    let declined = review(&server, &first, b"n\n");
    assert!(!declined.contains("synthetic-e2e-canary"));
    assert_eq!(pending(&server), first);
    assert!(
        !dir.path().join("calls").exists(),
        "declined review cannot invoke op"
    );
    let approved = review(&server, &first, b"y\n");
    assert!(!approved.contains("synthetic-e2e-canary"));
    eventually(|| dir.path().join("calls").exists().then_some(()));
    let mut surviving = consumer(&server);
    let second = pending(&server);
    canceled.0.kill().unwrap();
    canceled.0.wait().unwrap();
    let approved = review(&server, &second, b"y\n");
    assert!(!approved.contains("synthetic-e2e-canary"));
    std::fs::write(dir.path().join("release"), []).unwrap();
    let status = eventually(|| surviving.0.try_wait().unwrap());
    assert_eq!(
        status.code(),
        Some(37),
        "consumer exit status survives environment injection"
    );
    assert!(
        provider.0.try_wait().unwrap().is_none(),
        "one canceled exchange must not stop the provider"
    );
    assert_eq!(std::fs::read(dir.path().join("calls")).unwrap(), b"xx");
    let mut refused = consumer(&server);
    let third = pending(&server);
    server.success(&["deny", &third]);
    let status = eventually(|| refused.0.try_wait().unwrap());
    assert!(!status.success());
    assert_eq!(
        std::fs::read(dir.path().join("calls")).unwrap(),
        b"xx",
        "denial cannot invoke op"
    );
    for child in [&mut canceled, &mut surviving, &mut refused] {
        assert_no_secret_output(child);
    }
    drop(provider);
    assert!(
        !reader.join().unwrap(),
        "provider diagnostics leaked a value"
    );
}
