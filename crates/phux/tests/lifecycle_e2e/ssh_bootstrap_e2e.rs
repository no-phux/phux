//! `phux bootstrap` and `phux attach --ssh` through the real binary
//! (ADR-0120).
//!
//! Hermetic: private config, state, and runtime dirs, no overlay detection,
//! and a fake `ssh` via `$PHUX_SSH` that runs the remote half on this machine,
//! the way sshd would run it on the far end. `ssh -G` answers
//! `hostname 127.0.0.1`, so the QUIC dial that follows is a real loopback dial
//! to a real on-demand listener on a real auto-spawned server.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::Read as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, Dial, QuicDial};
use phux_protocol::caps::ServerFeature;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// How long a run has to reach the line a test waits for.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long to keep reading after that line, so what follows it is captured.
const SETTLE: Duration = Duration::from_millis(750);

/// What the fake ssh does when asked to run `phux bootstrap`.
#[derive(Clone, Copy)]
enum Remote {
    /// Run it, as a host with a current phux would.
    Current,
    /// Fail the way an older phux does on an unknown subcommand.
    TooOld,
    /// Fail the way ssh itself does when it cannot reach the host.
    Unreachable,
}

/// One scratch home per test: private dirs, a fake ssh, and a `phux` on the
/// fake remote's `PATH` that is this build.
struct Home {
    dir: TempDir,
}

impl Home {
    fn new(remote: Remote, hostname: &str) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("config/phux")).expect("config dir");
        std::fs::create_dir_all(root.join("run")).expect("runtime dir");
        std::fs::create_dir_all(root.join("bin")).expect("bin dir");
        std::os::unix::fs::symlink(PHUX, root.join("bin/phux")).expect("phux on the fake PATH");

        let bootstrap = match remote {
            Remote::Current => format!(
                "PATH=\"{}:$PATH\" exec sh -c \"$last\"",
                root.join("bin").display()
            ),
            Remote::TooOld => {
                "echo \"error: unrecognized subcommand 'bootstrap'\" >&2; exit 2".to_owned()
            }
            Remote::Unreachable => {
                "echo 'ssh: connect to host box port 22: Connection refused' >&2; exit 255"
                    .to_owned()
            }
        };
        let script = format!(
            "#!/bin/sh\n\
             # -G names the host; -T runs the remote command here; anything\n\
             # else is the `ssh -t` fallback, which just reports itself.\n\
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname {hostname}'; exit 0; fi\n\
             if [ \"$1\" = \"-T\" ]; then\n\
               for last in \"$@\"; do :; done\n\
               {bootstrap}\n\
             fi\n\
             echo \"FAKE SSH FALLBACK: $*\"\n"
        );
        let ssh = root.join("fake-ssh");
        std::fs::write(&ssh, script).expect("write fake ssh");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake ssh");
        }
        Self { dir }
    }

    fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let root = self.dir.path();
        vec![
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_STATE_HOME", root.join("state")),
            ("XDG_RUNTIME_DIR", root.join("run")),
            ("PHUX_PROFILE", PathBuf::from("default")),
            ("PHUX_TAILSCALE", root.join("no-such-tailscale")),
            ("PHUX_SSH", root.join("fake-ssh")),
        ]
    }

    /// Run `phux ARGS` on a PTY until `needle` appears or [`DEADLINE`]
    /// passes, then kill it. The PTY is what the attach's TTY preflight
    /// requires; reading on a thread keeps a quiet child from parking the
    /// test past its deadline.
    fn run_until(&self, args: &[&str], needle: &str) -> String {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(PHUX);
        cmd.args(args);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");
        let mut child = pty.slave.spawn_command(cmd).expect("spawn phux");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("clone reader");

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let start = Instant::now();
        let mut seen = String::new();
        let mut settle_until = None;
        loop {
            let budget = settle_until.map_or_else(
                || DEADLINE.saturating_sub(start.elapsed()),
                |until: Instant| until.saturating_duration_since(Instant::now()),
            );
            if budget.is_zero() {
                break;
            }
            match rx.recv_timeout(budget) {
                Ok(chunk) => {
                    seen.push_str(&String::from_utf8_lossy(&chunk));
                    if settle_until.is_none() && seen.contains(needle) {
                        settle_until = Some(Instant::now() + SETTLE);
                    }
                }
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        seen
    }
}

impl Drop for Home {
    /// Stop the server `phux bootstrap` auto-spawned, if any.
    fn drop(&mut self) {
        let _ = Command::new(PHUX)
            .envs(self.env())
            .args(["kill", "--server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Decode the token's hex into the bytes the QUIC preamble carries.
fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit pair"))
        .collect()
}

/// `phux bootstrap` starts the server, prints exactly one line, and that line
/// is everything a pinned QUIC dial needs.
#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn bootstrap_prints_one_line_that_is_enough_to_dial() {
    let home = Home::new(Remote::Current, "127.0.0.1");
    let out = Command::new(PHUX)
        .envs(home.env())
        .args([
            "bootstrap",
            "--client-version",
            "0.0.0-test",
            "--linger",
            "30",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run phux bootstrap");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "bootstrap failed: {stderr}");
    assert!(
        stderr.contains("the attaching client is 0.0.0-test"),
        "a version difference is named on stderr: {stderr}"
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "stdout is one line: {stdout:?}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one JSON document");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["linger_secs"], 30);
    assert_eq!(doc["phux_version"], env!("CARGO_PKG_VERSION"));
    assert!(doc["protocol_version"].is_string());
    let port = u16::try_from(doc["port"].as_u64().expect("port")).expect("u16");
    let token = doc["token"].as_str().expect("token");
    assert_eq!(token.len(), 64);
    let fingerprint = doc["cert_fingerprint"].as_str().expect("fingerprint");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let dial = Dial::Quic(QuicDial {
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
        server_name: "localhost".to_owned(),
        token: Some(unhex(token)),
        trust: CertTrust::Pinned(fingerprint.to_owned()),
    });
    let features = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), Connection::connect_dial(&dial))
            .await
            .expect("the dial completes")
            .expect("the listener admits the token it printed")
            .negotiated_bootstrap()
            .map(|n| n.server_features)
    });
    assert!(features.is_some_and(|caps| caps.contains(ServerFeature::OpenListener)));
}

/// The happy path: ssh bootstraps, and the attach rides QUIC to a real TUI.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn attach_over_ssh_bootstraps_and_attaches_over_quic() {
    let home = Home::new(Remote::Current, "127.0.0.1");
    let seen = home.run_until(&["attach", "--ssh", "me@box"], "\u{1b}[?1049h");
    assert!(
        seen.contains("bootstrapping me@box over ssh"),
        "the operator is told what is happening: {seen}"
    );
    assert!(
        seen.contains("attaching over QUIC to 127.0.0.1:"),
        "the dial goes to the host ssh -G named: {seen}"
    );
    assert!(
        seen.contains("\u{1b}[?1049h"),
        "the TUI came up over QUIC: {seen}"
    );
    assert!(
        !seen.contains("falling back"),
        "no fallback on the happy path: {seen}"
    );
}

/// A host whose phux predates `bootstrap` still gets an attach, the old way.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn attach_over_ssh_falls_back_when_the_host_cannot_bootstrap() {
    let home = Home::new(Remote::TooOld, "127.0.0.1");
    let seen = home.run_until(&["attach", "--ssh", "me@box"], "FAKE SSH FALLBACK");
    assert!(
        seen.contains("falling back"),
        "the fallback is announced: {seen}"
    );
    assert!(
        seen.contains("FAKE SSH FALLBACK: -t me@box phux attach"),
        "the fallback is `ssh -t HOST phux attach`: {seen}"
    );
}

/// When UDP cannot reach the listener, the probe falls back rather than
/// hanging on a dial that will never complete.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn attach_over_ssh_falls_back_when_udp_does_not_connect() {
    // TEST-NET-1: never routed, so the probe dial cannot succeed.
    let home = Home::new(Remote::Current, "192.0.2.1");
    let seen = home.run_until(&["attach", "--ssh", "me@box"], "FAKE SSH FALLBACK");
    assert!(
        seen.contains("QUIC to 192.0.2.1:") && seen.contains("did not connect"),
        "the probe names what it tried: {seen}"
    );
    assert!(
        seen.contains("FAKE SSH FALLBACK: -t me@box phux attach"),
        "{seen}"
    );
}

/// ssh itself failing is reported, not papered over with a fallback that
/// would fail the same way.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn attach_over_ssh_reports_an_unreachable_host_without_falling_back() {
    let home = Home::new(Remote::Unreachable, "127.0.0.1");
    let seen = home.run_until(&["attach", "--ssh", "me@box"], "failed (see above)");
    assert!(seen.contains("ssh to me@box failed"), "{seen}");
    assert!(
        !seen.contains("FAKE SSH FALLBACK"),
        "no fallback after ssh fails: {seen}"
    );
}

/// A sanity check on the fixture itself, so a broken fake ssh fails here and
/// not as a confusing attach failure.
#[test]
fn the_fake_ssh_answers_ssh_g_with_the_configured_host() {
    let home = Home::new(Remote::Current, "127.0.0.1");
    let out = Command::new(home.dir.path().join("fake-ssh"))
        .args(["-G", "--", "me@box"])
        .output()
        .expect("run fake ssh");
    assert!(String::from_utf8_lossy(&out.stdout).contains("hostname 127.0.0.1"));
}
