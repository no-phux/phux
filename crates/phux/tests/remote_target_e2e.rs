//! The `--remote` resolution ladder, driven through the real binary on a
//! real PTY (ADR-0093).
//!
//! `--remote` is an attach, so every rung sits behind the interactive TTY
//! preflight — which is why these tests open a PTY rather than piping. What
//! they pin is the *pairing* half of each rung, because that is the half
//! with side effects: which registry entry gets written, where the bearer
//! token lands and with what mode, and what the operator is told. The dial
//! that follows is the pre-existing `run_attach_remote` path and is not
//! re-tested here; each test stops as soon as the pairing it cares about is
//! observable, and kills the child.
//!
//! Network-free throughout. The ssh rung runs against a fake `ssh` via
//! `$PHUX_SSH` — the same seam `phux host enroll` is tested through — and
//! the `--code` rung contacts nothing at all.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// A 64-hex pairing token for the fake remote to mint.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A well-formed SHA-256 certificate fingerprint.
const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";

/// How long to wait for the pairing line before declaring the run stuck.
const DEADLINE: Duration = Duration::from_secs(20);

/// How long to keep draining after the needle appears, so the lines that
/// follow it are captured too.
///
/// The needle marks "the run has reached the point I care about", not "the
/// run has finished saying it" — a multi-line report arrives across several
/// PTY reads, and stopping on the first would assert against half a message.
const SETTLE: Duration = Duration::from_millis(750);

/// One scratch home per run: private config, private state, and a fake ssh.
struct RemoteHome {
    dir: TempDir,
}

impl RemoteHome {
    fn new() -> Self {
        Self {
            dir: TempDir::new().expect("tempdir"),
        }
    }

    /// A fake `ssh` answering the two commands `--remote`'s pairing rung
    /// issues. `overlay` empty means the host advertises nothing dialable,
    /// which is what drives the `ssh://` fallback.
    ///
    /// Note what is deliberately absent: `phux service install`. `--remote`
    /// must never install anything on the far end (ADR-0093 Decision 3), so
    /// this script fails the run if it is ever asked to.
    fn install_fake_ssh(&self, overlay: &str) -> PathBuf {
        let path = self.dir.path().join("fake-ssh");
        let overlay_json = if overlay.is_empty() {
            "[]".to_owned()
        } else {
            format!("[\"{overlay}\"]")
        };
        let script = format!(
            "#!/bin/sh\n\
             case \"$*\" in\n\
               *\"phux --version\"*) echo \"phux 0.0.0-test\" ;;\n\
               *\"phux pair --json\"*)\n\
                 printf '%s\\n' '{{\"token\":\"{TOKEN}\",\"cert_fingerprint\":\"{FINGERPRINT}\",\"overlay_addresses\":{overlay_json}}}' ;;\n\
               *\"phux service install\"*)\n\
                 echo \"fake ssh: --remote must not install a service\" >&2; exit 97 ;;\n\
               *) echo \"fake ssh: unexpected: $*\" >&2; exit 1 ;;\n\
             esac\n"
        );
        std::fs::write(&path, script).expect("write fake ssh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake ssh");
        }
        path
    }

    /// Run `phux <args...>` on a PTY and collect output until `needle`
    /// appears or [`DEADLINE`] elapses, then kill the child.
    ///
    /// Returns everything read. The attach that follows a successful pairing
    /// would block on a server that does not exist, so waiting for the child
    /// to exit is not an option — the needle IS the assertion point.
    ///
    /// The read runs on its own thread feeding a channel, and the deadline is
    /// enforced with `recv_timeout`. Reading inline would not work: a PTY read
    /// blocks until bytes arrive, so a child that goes quiet without exiting
    /// would park the test forever and the deadline would never be consulted.
    fn run_until(&self, args: &[&str], ssh: &Path, needle: &str) -> String {
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
        cmd.env("XDG_CONFIG_HOME", self.dir.path().join("config"));
        cmd.env("XDG_STATE_HOME", self.dir.path().join("state"));
        // Pin the RELEASED on-disk layout (`state/phux`, not `state/phux-dev`)
        // so the path assertions describe what a user actually sees (ADR-0080).
        cmd.env("PHUX_PROFILE", "default");
        cmd.env("PHUX_SSH", ssh);
        cmd.env("TERM", "xterm-256color");

        let mut child = pty.slave.spawn_command(cmd).expect("spawn phux");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("clone reader");

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        // Detached on purpose: it exits when the PTY closes after the kill
        // below, and nothing downstream needs to join it.
        std::thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let start = Instant::now();
        let mut seen = String::new();
        let mut settle_until = None;
        loop {
            // `saturating_duration_since` is already zero once the instant
            // has passed, which is the "stop now" signal the loop below reads.
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
                // Timeout during the settle window, or a disconnect (the child
                // closed the PTY): either way nothing more is coming.
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        seen
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config/phux/config.toml")).unwrap_or_default()
    }

    fn token_path(&self, name: &str) -> PathBuf {
        self.dir
            .path()
            .join("state/phux/remotes")
            .join(format!("{name}.token"))
    }
}

/// Assert a bearer token landed verbatim and owner-only.
fn assert_token(path: &Path) {
    assert_eq!(
        std::fs::read_to_string(path).expect("read token"),
        format!("{TOKEN}\n"),
        "the minted token must be stored verbatim"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .expect("stat token")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "a bearer token must be owner-only");
    }
}

/// The ssh rung on a host that advertises an overlay address: register a
/// pinned `quic://` entry under the `user@host` spelling, store the token
/// owner-only, and say so.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_registers_a_pinned_quic_entry_under_the_typed_name() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "paired");
    assert!(
        seen.contains("not registered") && seen.contains("pairing over ssh"),
        "the operator must be told what is happening before it happens; got: {seen}"
    );

    let config = home.config();
    assert!(
        config.contains("[[remote]]"),
        "pairing must write the remote registry; config={config}"
    );
    assert!(
        config.contains("name = \"me@mini\""),
        "the entry is keyed by the spelling the operator typed; config={config}"
    );
    assert!(
        config.contains("quic://100.64.0.7:8788"),
        "the overlay address plus the auto-listen port; config={config}"
    );
    assert!(
        config.contains(FINGERPRINT),
        "an unpinned routable entry would be refused at dial; config={config}"
    );
    assert_token(&home.token_path("me@mini"));
}

/// The ssh rung must not install a service on the far end: `phux host
/// enroll` does that, `--remote` does not (ADR-0093 Decision 3). The fake
/// ssh fails the whole run if it is ever asked to, so a clean pairing is
/// the proof.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_never_installs_a_service_on_the_far_end() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "paired");
    assert!(
        !seen.contains("must not install a service"),
        "--remote asked the far end to install a service; got: {seen}"
    );
    assert!(
        home.config().contains("quic://100.64.0.7:8788"),
        "pairing itself must still have succeeded; config={}",
        home.config()
    );
}

/// A host with nothing dialable degrades to an `ssh://` entry rather than
/// registering an endpoint that would fail at dial — and says so, naming
/// the verb that fixes it.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_degrades_to_an_ssh_entry_and_names_the_upgrade() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "ssh://");
    let config = home.config();
    // The user survives into the endpoint: the entry is dialed by re-execing
    // `ssh -t me@mini`, which needs the destination the operator typed.
    assert!(
        config.contains("endpoint = \"ssh://me@mini\""),
        "no dialable listener means an ssh:// entry naming the ssh destination; config={config}"
    );
    assert!(
        !home.token_path("me@mini").exists(),
        "an ssh:// entry rides ssh trust and must leave no bearer token behind"
    );
    assert!(
        seen.contains("phux host enroll"),
        "the degraded path must name the verb that upgrades it; got: {seen}"
    );
}

/// `--code` pairs from the same `phux://connect` link `phux pair --qr`
/// renders — contacting nothing. The fake ssh here is a path that does not
/// exist, so any ssh attempt fails the run.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn code_rung_registers_from_a_connect_link_without_ssh() {
    let home = RemoteHome::new();
    let no_ssh = home.dir.path().join("no-such-ssh");
    let link = format!("phux://connect?url=wss://100.64.0.7:8787&fp={FINGERPRINT}&token={TOKEN}");

    let seen = home.run_until(
        &["attach", "--remote", "mini", "--code", &link],
        &no_ssh,
        "paired",
    );

    let config = home.config();
    assert!(
        config.contains("name = \"mini\"") && config.contains("wss://100.64.0.7:8787"),
        "the link's own endpoint is what gets registered; config={config}"
    );
    assert!(config.contains(FINGERPRINT), "config={config}");
    assert_token(&home.token_path("mini"));
    assert!(
        seen.contains("needs no code"),
        "the operator should learn the code is one-time; got: {seen}"
    );
}

/// A malformed code is refused before anything is written. A half-registered
/// host with an orphaned bearer token would be worse than a clean failure.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn a_bad_code_registers_nothing() {
    let home = RemoteHome::new();
    let no_ssh = home.dir.path().join("no-such-ssh");

    let seen = home.run_until(
        &[
            "attach",
            "--remote",
            "mini",
            "--code",
            "phux://connect?url=wss://x",
        ],
        &no_ssh,
        "--code",
    );
    assert!(
        seen.contains("token"),
        "the refusal names what is missing; got: {seen}"
    );
    assert!(
        !home.config().contains("[[remote]]"),
        "a rejected code must not leave a registry entry; config={}",
        home.config()
    );
    assert!(
        !home.token_path("mini").exists(),
        "a rejected code must not leave a bearer token"
    );
}

/// `--no-enroll` refuses an unregistered host outright, and names both
/// remedies rather than failing bare.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn no_enroll_refuses_an_unregistered_host_with_both_remedies() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(
        &["attach", "--remote", "me@mini", "--no-enroll"],
        &ssh,
        "not a registered host",
    );
    assert!(
        seen.contains("--code") && seen.contains("phux host enroll"),
        "the refusal must name both remedies; got: {seen}"
    );
    assert!(
        !home.config().contains("[[remote]]"),
        "--no-enroll must not pair; config={}",
        home.config()
    );
}
