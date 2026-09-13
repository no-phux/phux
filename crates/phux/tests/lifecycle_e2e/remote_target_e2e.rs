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

    /// A fake `ssh` answering the three commands `--remote`'s bootstrap rung
    /// issues. `overlay` empty means the host advertises nothing dialable,
    /// which is what drives the `ssh://` fallback. Every invocation is logged
    /// so tests can prove the remote service starts before pairing.
    fn install_fake_ssh(&self, overlay: &str) -> PathBuf {
        self.install_fake_ssh_with_service(overlay, "echo \"service installed\"")
    }

    fn install_fake_ssh_with_service(&self, overlay: &str, service: &str) -> PathBuf {
        let path = self.dir.path().join("fake-ssh");
        let overlay_json = if overlay.is_empty() {
            "[]".to_owned()
        } else {
            format!("[\"{overlay}\"]")
        };
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$PHUX_TEST_SSH_CALLS\"\n\
             case \"$*\" in\n\
               *\"phux --version\"*) echo \"phux 0.0.0-test\" ;;\n\
               *\"phux service install\"*) {service} ;;\n\
               *\"phux pair --json\"*)\n\
                 printf '%s\\n' '{{\"token\":\"{TOKEN}\",\"cert_fingerprint\":\"{FINGERPRINT}\",\"overlay_addresses\":{overlay_json}}}' ;;\n\
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
        cmd.env("PHUX_TEST_SSH_CALLS", self.dir.path().join("ssh-calls"));
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

    fn register_direct(&self, name: &str, endpoint: &str) {
        let config = self.dir.path().join("config/phux/config.toml");
        std::fs::create_dir_all(config.parent().expect("config parent"))
            .expect("create config parent");
        let token = self.token_path(name);
        std::fs::create_dir_all(token.parent().expect("token parent"))
            .expect("create token parent");
        std::fs::write(&token, format!("{TOKEN}\n")).expect("write token");
        std::fs::write(
            config,
            format!(
                "[[remote]]\nname = {name:?}\nendpoint = {endpoint:?}\ntoken-file = {:?}\n",
                token.display().to_string()
            ),
        )
        .expect("write config");
    }

    fn ssh_calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ssh-calls")).unwrap_or_default()
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
        seen.contains("pairing mini over ssh")
            && seen.contains("installing and starting the remote Phux service"),
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

/// A registry entry from the old pair-only behavior can name a server that is
/// not running. The ordinary `--remote` spelling repairs that cold host over
/// ssh, rewrites the entry with fresh credentials, and then attaches.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn registered_but_unreachable_host_is_started_and_repaired_over_ssh() {
    let home = RemoteHome::new();
    home.register_direct("me@mini", "wss://127.0.0.1:9");
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "paired");
    assert!(
        seen.contains("registered host mini could not establish a direct attach")
            && seen.contains("repairing it over ssh"),
        "a cold registered host must enter the repair rung; got: {seen}"
    );
    assert!(
        home.config().contains("quic://100.64.0.7:8788"),
        "repair must replace the dead endpoint; config={}",
        home.config()
    );
    assert!(
        home.ssh_calls().contains("phux service install"),
        "repair must start the remote service"
    );
}

/// `--no-enroll` is also the no-repair boundary for an existing dead entry:
/// it may attempt the saved endpoint, but it must not shell into the host.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn no_enroll_does_not_repair_an_unreachable_registered_host() {
    let home = RemoteHome::new();
    home.register_direct("me@mini", "wss://127.0.0.1:9");
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(
        &["attach", "--remote", "me@mini", "--no-enroll"],
        &ssh,
        "WebSocket attach",
    );
    assert!(
        seen.contains("failed"),
        "the saved dead endpoint should fail without repair; got: {seen}"
    );
    assert_eq!(
        home.ssh_calls(),
        "",
        "--no-enroll must not invoke ssh for repair"
    );
}

/// A first interactive remote attach provisions the same per-user service as
/// `phux host enroll`, before it mints credentials. That order matters: the
/// endpoint written locally must describe a server that is already starting.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_starts_the_remote_service_before_pairing() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("100.64.0.7");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "paired");
    let calls = home.ssh_calls();
    let version = calls.find("phux --version").expect("version probe");
    let service = calls
        .find("phux service install --quic 0.0.0.0:8788")
        .expect("service install");
    let pair = calls.find("phux pair --json").expect("pairing");
    assert!(
        version < service && service < pair,
        "expected version probe, service start, then pairing; calls={calls:?}"
    );
    assert!(
        seen.contains("installing and starting the remote Phux service"),
        "the side effect must be visible before it happens; got: {seen}"
    );
}

/// A host with nothing directly dialable uses an `ssh://` entry rather than
/// registering an endpoint that would fail at dial. The installed service
/// still owns the remote work after the ssh transport disconnects.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_uses_ssh_when_the_installed_service_has_no_direct_endpoint() {
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
        seen.contains("installed service still keeps its work alive"),
        "the ssh route must explain that remote work remains durable; got: {seen}"
    );
}

/// A service-manager refusal must not leave a direct registry entry pointing
/// at a listener that was never started. Pairing still supplies credentials,
/// but the resulting ssh route reaches the ordinary remote auto-spawn path.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn service_install_failure_falls_back_to_remote_auto_spawn_over_ssh() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh_with_service(
        "100.64.0.7",
        "echo 'service manager unavailable' >&2; exit 97",
    );

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "ssh://");
    let config = home.config();
    assert!(
        config.contains("endpoint = \"ssh://me@mini\""),
        "an unstarted direct listener must not be registered; config={config}"
    );
    assert!(
        !home.token_path("me@mini").exists(),
        "the ssh route must not retain an unused bearer token"
    );
    assert!(
        seen.contains("remote service install failed")
            && seen.contains("auto-starts an unsupervised server"),
        "the fallback and its durability limit must be explicit; got: {seen}"
    );
    assert!(
        home.ssh_calls().contains("phux pair --json"),
        "pairing should still complete before the ssh route is recorded"
    );
}

/// `--code` pairs from the same `https://phux.phall.io/connect` link
/// `phux pair --qr` renders — contacting nothing. The fake ssh here is a path that does not
/// exist, so any ssh attempt fails the run.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn code_rung_registers_from_a_connect_link_without_ssh() {
    let home = RemoteHome::new();
    let no_ssh = home.dir.path().join("no-such-ssh");
    let link = format!(
        "https://phux.phall.io/connect?url=wss://100.64.0.7:8787&fp={FINGERPRINT}&token={TOKEN}"
    );

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
            "https://phux.phall.io/connect?url=wss://x",
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
