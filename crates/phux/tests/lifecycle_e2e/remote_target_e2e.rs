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
//! `$PHUX_SSH` — the same seam `phux host add` is tested through — and
//! the `--code` rung contacts nothing at all. The direct-route probe dials
//! a TEST-NET address under a short `PHUX_DIRECT_PROBE_TIMEOUT_MS`, so
//! every ssh pairing here ends on the `ssh://` route with the candidate
//! kept as `direct`; the answering case lives in `host_add_e2e.rs`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// TEST-NET-3 (RFC 5737): never routed, so a probe cannot reach a real
/// machine and cannot succeed.
const OVERLAY: &str = "203.0.113.7";

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
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname {OVERLAY}'; exit 0; fi\n\
             case \"$*\" in\n\
               *\"phux --version\"*) echo \"phux 0.0.0-test\" ;;\n\
               *\"phux service install\"*) {service} ;;\n\
               *\"phux server --ensure\"*) echo \"ensured\" ;;\n\
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
        // Drop inherited `PHUX_SOCKET` / `PHUX_WS_*` from a live pane
        // (phux-lru0). CommandBuilder has no env_remove; rebuild the table.
        cmd.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            cmd.env("PATH", path);
        }
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            cmd.env("TMPDIR", tmp);
        }
        cmd.env("HOME", self.dir.path());
        cmd.env("XDG_CONFIG_HOME", self.dir.path().join("config"));
        cmd.env("XDG_STATE_HOME", self.dir.path().join("state"));
        // Pin the RELEASED on-disk layout (`state/phux`, not `state/phux-dev`)
        // so the path assertions describe what a user actually sees (ADR-0080).
        cmd.env("PHUX_PROFILE", "default");
        cmd.env("PHUX_SSH", ssh);
        cmd.env("PHUX_TAILSCALE", self.dir.path().join("no-such-tailscale"));
        cmd.env("PHUX_TEST_SSH_CALLS", self.dir.path().join("ssh-calls"));
        // The probe dials a TEST-NET address: fail it fast.
        cmd.env("PHUX_DIRECT_PROBE_TIMEOUT_MS", "300");
        cmd.env("TERM", "xterm-256color");
        // Everything below closes a door `PHUX_PROFILE=default` opens
        // (phux-vlv1). The released profile is not just a path layout: it is
        // also the local socket the operator's own server is on, and the
        // gate that makes a server auto-bind the host's overlay port.
        //
        // * `PHUX_SOCKET` moves the local instance inside this scratch home.
        // * `PHUX_NO_AUTO_LISTEN` is the documented opt-out from the
        //   auto-overlay bind (ADR-0081).
        // * `PHUX_TAILSCALE` above, pointed at a program that cannot exist,
        //   turns overlay detection off; setting it also suppresses the
        //   CGNAT route heuristic.
        cmd.env("PHUX_SOCKET", self.dir.path().join("phux.sock"));
        cmd.env("PHUX_NO_AUTO_LISTEN", "1");

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

/// The ssh rung on a host that advertises an overlay address: register the
/// entry under the `user@host` spelling with the pinned `quic://` candidate
/// kept as `direct` (nothing answers here), store the token owner-only,
/// and narrate every step as it happens.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_registers_a_pinned_entry_under_the_typed_name() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh(OVERLAY);

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "registered me@mini");
    assert!(
        seen.contains("setting mini up over ssh")
            && seen.contains("found over ssh")
            && seen.contains("server running, supervised by its service unit")
            && seen.contains("paired"),
        "the operator must be told what is happening as it happens; got: {seen}"
    );

    let config = home.config();
    assert!(
        config.contains("[[remote]]"),
        "pairing must write the remote registry; config={config}"
    );
    assert!(
        config.contains("name = \"me@mini\"") && config.contains("ssh = \"me@mini\""),
        "the entry is keyed by the spelling the operator typed and remembers it for ssh; config={config}"
    );
    assert!(
        config.contains(&format!("direct = \"quic://{OVERLAY}:8788\"")),
        "the overlay address plus the auto-listen port is kept to promote; config={config}"
    );
    assert!(
        config.contains(FINGERPRINT),
        "an unpinned routable entry would be refused at dial; config={config}"
    );
    assert_token(&home.token_path("me@mini"));
}

/// A registered entry can name a server that is not running. The ordinary
/// `--remote` spelling walks the repair ladder over ssh: start the server
/// and retry the saved route first, and only when that still fails re-pair
/// and rewrite the entry. `phux attach NAME` takes the same ladder.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn registered_but_unreachable_host_is_started_then_repaired_over_ssh() {
    for args in [
        ["--remote", "me@mini"].as_slice(),
        ["attach", "me@mini"].as_slice(),
    ] {
        let home = RemoteHome::new();
        home.register_direct("me@mini", "wss://127.0.0.1:9");
        let ssh = home.install_fake_ssh(OVERLAY);

        let seen = home.run_until(args, &ssh, "registered me@mini");
        assert!(
            seen.contains("me@mini is not answering at wss://127.0.0.1:9")
                && seen.contains("starting its server over ssh (me@mini)"),
            "args={args:?}: a cold registered host must be started first; got: {seen}"
        );
        assert!(
            seen.contains("still does not answer with the saved credentials; re-pairing"),
            "args={args:?}: only after the retry fails is the host re-paired; got: {seen}"
        );
        let calls = home.ssh_calls();
        let start = calls
            .find("phux service install")
            .expect("repair must start the remote service");
        let pair = calls.find("phux pair --json").expect("then re-pair");
        assert!(
            start < pair,
            "args={args:?}: start before re-pair; calls={calls:?}"
        );
        assert!(
            home.config()
                .contains(&format!("direct = \"quic://{OVERLAY}:8788\""))
                && !home.config().contains("wss://127.0.0.1:9"),
            "args={args:?}: repair must replace the dead endpoint; config={}",
            home.config()
        );
    }
}

/// `--no-enroll` is also the no-repair boundary for an existing dead entry:
/// it may attempt the saved endpoint, but it must not shell into the host.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn no_enroll_does_not_repair_an_unreachable_registered_host() {
    let home = RemoteHome::new();
    home.register_direct("me@mini", "wss://127.0.0.1:9");
    let ssh = home.install_fake_ssh(OVERLAY);

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
/// `phux host add`, before it mints credentials. That order matters: the
/// endpoint written locally must describe a server that is already running.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_starts_the_remote_service_before_pairing() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh(OVERLAY);

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "registered me@mini");
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
        seen.contains("server running, supervised by its service unit"),
        "the side effect must be visible as it happens; got: {seen}"
    );
}

/// A host with nothing directly dialable uses an `ssh://` entry rather than
/// registering an endpoint that would fail at dial, and says which routes
/// it tried. The host ssh connects to is still worth one dial, so it is the
/// candidate kept.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn ssh_rung_uses_ssh_when_no_direct_route_answers() {
    let home = RemoteHome::new();
    let ssh = home.install_fake_ssh("");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "registered me@mini");
    let config = home.config();
    // The user survives into the endpoint: the entry is dialed by re-execing
    // `ssh -t me@mini`, which needs the destination the operator typed.
    assert!(
        config.contains("endpoint = \"ssh://me@mini\""),
        "no answering listener means an ssh:// entry naming the ssh destination; config={config}"
    );
    assert!(
        config.contains(&format!("direct = \"quic://{OVERLAY}:8788\"")),
        "the host ssh -G named is kept as the candidate; config={config}"
    );
    assert!(
        seen.contains("no direct route answered")
            && seen.contains(&format!("tried quic://{OVERLAY}:8788"))
            && seen.contains("every attach tries the direct route first"),
        "the ssh route must explain itself; got: {seen}"
    );
}

/// A service-manager refusal still gets the host a server — unsupervised,
/// through `phux server --ensure` — and the operator is told it will not
/// survive a reboot. Pairing still completes.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn service_install_failure_falls_back_to_an_unsupervised_server() {
    let home = RemoteHome::new();
    let ssh = home
        .install_fake_ssh_with_service(OVERLAY, "echo 'service manager unavailable' >&2; exit 97");

    let seen = home.run_until(&["--remote", "me@mini"], &ssh, "registered me@mini");
    assert!(
        seen.contains(
            "server running unsupervised (service install failed: service manager unavailable)"
        ) && seen.contains("will not come back by itself after a reboot"),
        "the fallback and its durability limit must be explicit; got: {seen}"
    );
    let calls = home.ssh_calls();
    let ensure = calls
        .find("phux server --ensure")
        .expect("an unsupervised start");
    let pair = calls
        .find("phux pair --json")
        .expect("pairing still completes");
    assert!(
        ensure < pair,
        "the server is up before pairing; calls={calls:?}"
    );
}

/// `--code` pairs from the same `https://phux.sh/connect` link
/// `phux pair --qr` renders — contacting nothing. The fake ssh here is a path that does not
/// exist, so any ssh attempt fails the run.
#[test]
#[ignore = "spawns a PTY-backed binary; runs in the e2e lane"]
fn code_rung_registers_from_a_connect_link_without_ssh() {
    let home = RemoteHome::new();
    let no_ssh = home.dir.path().join("no-such-ssh");
    let link = format!(
        "https://phux.sh/connect?url=wss://100.64.0.7:8787&fp={FINGERPRINT}&token={TOKEN}"
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
            "https://phux.sh/connect?url=wss://x",
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
    let ssh = home.install_fake_ssh(OVERLAY);

    let seen = home.run_until(
        &["attach", "--remote", "me@mini", "--no-enroll"],
        &ssh,
        "not a registered host",
    );
    assert!(
        seen.contains("--code") && seen.contains("phux host add"),
        "the refusal must name both remedies; got: {seen}"
    );
    assert!(
        !home.config().contains("[[remote]]"),
        "--no-enroll must not pair; config={}",
        home.config()
    );
}
