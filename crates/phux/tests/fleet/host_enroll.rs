//! The ssh form of `phux host add`, pinned at the binary level
//! (ADR-0122; formerly `phux host enroll`, phux-i0e8.12.7).
//!
//! These tests drive the REAL binary through both role tails, network-free:
//!
//!   * the full ssh path runs against a fake `ssh` via `$PHUX_SSH` — the
//!     same seam the federation hub's satellite dialer uses — which answers
//!     `phux --version`, `phux service install`, `phux server --ensure`,
//!     `phux pair --json`, `phux upgrade`, and `ssh -G` from a script and
//!     logs every call;
//!   * the direct-route probe dials TEST-NET-3 addresses that can never
//!     answer, under a short `PHUX_DIRECT_PROBE_TIMEOUT_MS`, so every run
//!     ends on the `ssh://` fallback with the candidate kept as `direct`.
//!     The path where a probe answers needs a real listener and lives in
//!     the e2e lane (`host_add_e2e.rs`);
//!   * `--ssh-only` must never contact the host at all, so its `$PHUX_SSH`
//!     points at a path that does not exist: any ssh attempt fails the run.
//!
//! What they pin: each role registers into ITS registry (`[[remote]]` vs
//! `[[satellites]]` in the one config.toml) with the pairing token under
//! the role-correct state directory (`remotes/` vs `satellites/`); the
//! order of the ssh steps; the `--adopt` retry when a server is already
//! live; the one-time legacy token-store migration; the three failure
//! transcripts (ssh unreachable, no phux, no direct route); `--ssh-only`
//! registers `ssh://HOST` and leaves no credential behind; and the `--json`
//! success document is the documented `schema_version`-1 `"host"` wrapper.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::Path;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// A 64-hex pairing token for the fake remote to mint.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A well-formed SHA-256 certificate fingerprint.
const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";
/// TEST-NET-3 (RFC 5737): never routed, so a probe dial cannot succeed and
/// never reaches a real machine.
const OVERLAY: &str = "203.0.113.7";
/// The host the fake `ssh -G` names, also unroutable.
const SSH_HOST: &str = "203.0.113.9";

/// How the fake remote answers `phux service install`.
#[derive(Clone, Copy)]
enum Install {
    /// The unit is written and loaded.
    Ok,
    /// A live server holds the socket; the refusal that asks for `--adopt`.
    IncumbentLive,
    /// No service manager at all.
    Unavailable,
}

/// How the fake remote answers `phux pair --json`.
#[derive(Clone, Copy)]
enum Pair {
    Ok,
    /// The store predates versioning; `--migrate-legacy` is required.
    LegacyStore,
}

/// One scratch home for a single run: private config, state, and (when the
/// flow is allowed to "ssh") a fake `ssh` answering from a script.
struct EnrollHome {
    dir: TempDir,
}

impl EnrollHome {
    fn new() -> Self {
        Self {
            dir: TempDir::new().expect("tempdir"),
        }
    }

    /// Write the fake `ssh` and return its path. It answers every command
    /// the ssh form issues; anything else fails the run.
    fn install_fake_ssh(&self) -> std::path::PathBuf {
        self.install_fake_ssh_with(Install::Ok, Pair::Ok)
    }

    fn install_fake_ssh_with(&self, install: Install, pair: Pair) -> std::path::PathBuf {
        let install = match install {
            Install::Ok => "echo \"service installed\"".to_owned(),
            Install::IncumbentLive => {
                "case \"$*\" in *--adopt*) echo \"unit armed\" ;; *) echo \"phux service: a server is already running on /tmp/x.sock\" >&2; exit 1 ;; esac".to_owned()
            }
            Install::Unavailable => {
                "echo 'phux service: no unit generator for this platform' >&2; exit 1".to_owned()
            }
        };
        let pair = match pair {
            Pair::Ok => "true".to_owned(),
            Pair::LegacyStore => "case \"$*\" in *--migrate-legacy*) ;; *) echo 'phux pair: failed to mint token: legacy token store requires explicit migration' >&2; exit 1 ;; esac".to_owned(),
        };
        let path = self.dir.path().join("fake-ssh");
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$PHUX_TEST_SSH_CALLS\"\n\
             # argv: -G -- HOST, or -o BatchMode=yes HOST phux <subcommand...>\n\
             if [ \"$1\" = \"-G\" ]; then echo 'user me'; echo 'hostname {SSH_HOST}'; exit 0; fi\n\
             case \"$*\" in\n\
               *\"phux --version\"*) echo \"phux 0.0.0-test\" ;;\n\
               *\"phux service install\"*) {install} ;;\n\
               *\"phux server --ensure\"*) echo \"ensured\" ;;\n\
               *\"phux upgrade\"*) echo \"upgrading\" ;;\n\
               *\"phux pair --json\"*)\n\
                 {pair}\n\
                 printf '%s\\n' '{{\"token\":\"{TOKEN}\",\"cert_fingerprint\":\"{FINGERPRINT}\",\"overlay_addresses\":[\"{OVERLAY}\"]}}' ;;\n\
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

    /// A fake `ssh` that fails the way ssh does when it cannot reach the
    /// host (exit 255), or the way a remote shell does when `phux` is not
    /// there (exit 127).
    fn install_failing_ssh(&self, code: u8, stderr: &str) -> std::path::PathBuf {
        let path = self.dir.path().join("fake-ssh");
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$PHUX_TEST_SSH_CALLS\"\n\
             echo '{stderr}' >&2\n\
             exit {code}\n"
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

    /// Put no-op init-system clients first on `PATH` so a Linux CI runner can
    /// prove the unit was armed without requiring a live user systemd session.
    /// The test must never address the developer's real service manager.
    fn isolated_path(&self) -> std::ffi::OsString {
        let bin = self.dir.path().join("fake-bin");
        std::fs::create_dir_all(&bin).expect("create fake init-tool dir");
        for tool in ["launchctl", "systemctl"] {
            let path = bin.join(tool);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake init tool");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod fake init tool");
            }
        }
        let mut paths = vec![bin];
        if let Some(inherited) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&inherited));
        }
        std::env::join_paths(paths).expect("construct isolated PATH")
    }

    /// Run `phux <args...>` against this home's private config and state,
    /// with `$PHUX_SSH` pointed at `ssh` (a missing path proves the run
    /// never sshed). Returns `(exit_code, stdout, stderr)`.
    ///
    /// `PHUX_PROFILE=default` pins the *released* on-disk layout
    /// (`state/phux`, not `state/phux-dev`). The binary under test is a debug
    /// build, so it would otherwise resolve the `dev` profile and this file's
    /// path assertions would be describing a layout no user ever sees
    /// (ADR-0080).
    ///
    /// `HOME` is redirected too: `--role satellite` writes or patches this
    /// machine's service unit, and without a sandbox that lands in the
    /// developer's real `~/Library/LaunchAgents` (or systemd user dir).
    fn run(&self, args: &[&str], ssh: &Path) -> (i32, String, String) {
        let out = crate::common::phux_cmd(PHUX)
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("PHUX_PROFILE", "default")
            .env("PHUX_SSH", ssh)
            .env("PHUX_TEST_SSH_CALLS", self.dir.path().join("ssh-calls"))
            // The probe dials TEST-NET addresses: fail them fast.
            .env("PHUX_DIRECT_PROBE_TIMEOUT_MS", "300")
            .env("PATH", self.isolated_path())
            .args(args)
            .output()
            .expect("run phux binary");
        let stderr = String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|line| !line.starts_with("dhat: "))
            .fold(String::new(), |mut acc, line| {
                acc.push_str(line);
                acc.push('\n');
                acc
            });
        (
            out.status.code().expect("phux exited via code, not signal"),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr,
        )
    }

    /// The one registry file both roles share.
    fn config(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("config/phux/config.toml"))
            .expect("read config.toml")
    }

    /// Every ssh invocation, one per line, in order.
    fn ssh_calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ssh-calls")).unwrap_or_default()
    }

    /// Where a role's pairing token must land: `remotes/<name>.token` or
    /// `satellites/<name>.token` under the phux state dir.
    fn token_path(&self, role_dir: &str, name: &str) -> std::path::PathBuf {
        self.dir
            .path()
            .join("state/phux")
            .join(role_dir)
            .join(format!("{name}.token"))
    }

    /// Assert the pairing token landed at `path`, owner-only, and nowhere
    /// under `absent_role_dir`.
    fn assert_token_routed(&self, path: &Path, absent_role_dir: &str) {
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
        assert!(
            !self
                .dir
                .path()
                .join("state/phux")
                .join(absent_role_dir)
                .exists(),
            "the other role's token directory must stay untouched"
        );
    }

    /// The per-user service unit `--role satellite` patches or writes.
    ///
    /// macOS reads `$HOME/Library/LaunchAgents`; Linux reads
    /// `$XDG_CONFIG_HOME/systemd/user`. Both `HOME` and `XDG_CONFIG_HOME` are
    /// the tempdir (see [`Self::run`]).
    fn hub_unit_path(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.dir
                .path()
                .join("Library/LaunchAgents/com.phux.server.plist")
        } else {
            self.dir.path().join("config/systemd/user/phux.service")
        }
    }

    fn write_hub_unit(&self, body: &str) {
        let path = self.hub_unit_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create unit dir");
        }
        std::fs::write(path, body).expect("write unit");
    }
}

/// Direct-exec unit with a QUIC listener and a socket override, no `--hub`.
/// Those two flags are exactly what a reinstall would drop (ADR-0083).
const fn unit_without_hub() -> &'static str {
    if cfg!(target_os = "macos") {
        "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.phux.server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/phux</string>
    <string>server</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PHUX_QUIC_ADDR</key>
    <string>0.0.0.0:8788</string>
    <key>PHUX_SOCKET</key>
    <string>/tmp/custom/phux.sock</string>
  </dict>
</dict>
</plist>
"
    } else {
        "\
[Service]
Type=simple
ExecStart=/usr/local/bin/phux server
Environment=\"PHUX_QUIC_ADDR=0.0.0.0:8788\"
Environment=\"PHUX_SOCKET=/tmp/custom/phux.sock\"
"
    }
}

fn unit_with_hub() -> String {
    if cfg!(target_os = "macos") {
        unit_without_hub().replace(
            "<string>server</string>",
            "<string>server</string>\n    <string>--hub</string>",
        )
    } else {
        unit_without_hub().replace(
            "ExecStart=/usr/local/bin/phux server",
            "ExecStart=/usr/local/bin/phux server --hub",
        )
    }
}

fn assert_token_never_printed(stdout: &str, stderr: &str) {
    assert!(
        !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
        "the pairing token must not appear in argv, config, or logs; \
         stdout={stdout} stderr={stderr}"
    );
}

fn unit_kept_existing_flags(body: &str) {
    assert!(
        body.contains("0.0.0.0:8788") && body.contains("/tmp/custom/phux.sock"),
        "existing listener/socket flags must survive --hub ensure:\n{body}"
    );
    if cfg!(target_os = "macos") {
        assert!(
            body.contains("<string>--hub</string>"),
            "expected --hub in ProgramArguments:\n{body}"
        );
    } else {
        assert!(
            body.contains("ExecStart=/usr/local/bin/phux server --hub"),
            "expected --hub on ExecStart:\n{body}"
        );
    }
}

/// The default role's tail: `[[remote]]` in the registry, the token under
/// `remotes/`, and — with no direct route answering — an `ssh://` endpoint
/// that remembers the ssh destination and keeps the first candidate as
/// `direct` for a later attach to promote.
#[test]
fn add_remote_registers_remote_registry_and_remote_token_dir() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");

    let config = home.config();
    assert!(
        config.contains("[[remote]]") && !config.contains("[[satellites]]"),
        "a remote enrollment must land in the remote registry only; config={config}"
    );
    assert!(
        config.contains("name = \"mini\"") && config.contains("endpoint = \"ssh://me@mini\""),
        "the entry carries the default name and, with nothing answering, the ssh route; \
         config={config}"
    );
    assert!(
        config.contains("ssh = \"me@mini\""),
        "the ssh destination is remembered for repairs; config={config}"
    );
    assert!(
        config.contains(&format!("direct = \"quic://{OVERLAY}:8788\"")),
        "the first candidate is kept for a later attach to promote; config={config}"
    );
    assert!(
        config.contains(FINGERPRINT),
        "the reported certificate fingerprint must be pinned; config={config}"
    );
    home.assert_token_routed(&home.token_path("remotes", "mini"), "satellites");
    assert_token_never_printed(&stdout, &stderr);
    assert!(
        !home.hub_unit_path().exists(),
        "--role remote must not write a local hub unit"
    );

    // The operator is told what happened and what to type next.
    assert!(
        stdout.contains("Registered mini -> ssh://me@mini") && stdout.contains("phux attach mini"),
        "stdout={stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "tried quic://{OVERLAY}:8788, quic://{SSH_HOST}:8788"
        )),
        "every route tried is named; stdout={stdout}"
    );
    for step in [
        "mini: phux 0.0.0-test found over ssh",
        "mini: server running, supervised by its service unit",
        "mini: paired",
        &format!("mini: trying quic://{OVERLAY}:8788"),
    ] {
        assert!(
            stderr.contains(step),
            "missing progress line {step:?}: {stderr}"
        );
    }
}

/// The ssh steps run in the order an operator would do them by hand:
/// confirm phux, start the server, pair, then find the address ssh
/// connects to. Pairing after the service install is what makes the
/// listener's environment visible to `phux pair`.
#[test]
fn add_runs_the_ssh_steps_in_order() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, _stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    let calls = home.ssh_calls();
    let version = calls.find("phux --version").expect("version probe");
    let service = calls
        .find("phux service install --quic 0.0.0.0:8788")
        .expect("service install");
    let pair = calls.find("phux pair --json").expect("pairing");
    let hostname = calls.find("-G -- me@mini").expect("ssh -G");
    assert!(
        version < service && service < pair && pair < hostname,
        "expected version probe, service start, pairing, then -G; calls={calls:?}"
    );
    assert!(
        !calls.contains("--adopt") && !calls.contains("server --ensure"),
        "a clean install needs neither fallback; calls={calls:?}"
    );
}

/// `--remote-phux` and `:PORT` on the target reach the far end: the named
/// binary runs there, and the port is what the service binds and the
/// probe dials.
#[test]
fn add_honors_remote_phux_and_the_target_port() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, _stdout, stderr) = home.run(
        &[
            "host",
            "add",
            "me@mini:9443",
            "--remote-phux",
            "/opt/homebrew/bin/phux",
        ],
        &ssh,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    let calls = home.ssh_calls();
    assert!(
        calls.contains("/opt/homebrew/bin/phux --version")
            && calls.contains("/opt/homebrew/bin/phux service install --quic 0.0.0.0:9443")
            && calls.contains("/opt/homebrew/bin/phux pair --json"),
        "calls={calls:?}"
    );
    assert!(
        home.config()
            .contains(&format!("direct = \"quic://{OVERLAY}:9443\"")),
        "the port travels into the candidate; config={}",
        home.config()
    );
}

/// A server already holding the socket makes `service install` refuse;
/// the refusal is answered with `--adopt`, so the live server keeps its
/// panes and supervision takes over its next start.
#[test]
fn add_adopts_a_server_that_is_already_running() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh_with(Install::IncumbentLive, Pair::Ok);

    let (code, _stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    let calls = home.ssh_calls();
    assert!(
        calls.contains("phux service install --quic 0.0.0.0:8788 --adopt"),
        "the refusal must be answered with --adopt; calls={calls:?}"
    );
    assert!(
        stderr.contains("server already running; its service unit is armed"),
        "the operator learns the server was kept; stderr={stderr}"
    );
    assert!(
        !calls.contains("server --ensure"),
        "a live server needs no ensure; calls={calls:?}"
    );
}

/// No service manager: the server is still started, unsupervised, and the
/// operator is told it will not survive a reboot. `--no-service` takes
/// the same path on purpose.
#[test]
fn add_falls_back_to_an_unsupervised_server() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh_with(Install::Unavailable, Pair::Ok);

    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        home.ssh_calls().contains("phux server --ensure"),
        "an install failure still starts a server; calls={:?}",
        home.ssh_calls()
    );
    assert!(
        stderr.contains("server running unsupervised (service install failed")
            && stdout.contains("will not come back by itself after a reboot"),
        "the durability limit must be explicit; stdout={stdout} stderr={stderr}"
    );

    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();
    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini", "--no-service"], &ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    let calls = home.ssh_calls();
    assert!(
        calls.contains("phux server --ensure") && !calls.contains("service install"),
        "--no-service starts a server without installing a unit; calls={calls:?}"
    );
    assert!(
        stdout.contains("--no-service") && stdout.contains("will not come back"),
        "stdout={stdout}"
    );
}

/// A token store that predates versioning is migrated once, the server is
/// asked to restart so its listeners re-read it, and pairing proceeds.
#[test]
fn add_migrates_a_legacy_token_store_once() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh_with(Install::Ok, Pair::LegacyStore);

    let (code, _stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    let calls = home.ssh_calls();
    let plain = calls.find("phux pair --json\n").expect("first pair");
    let migrated = calls
        .find("phux pair --json --migrate-legacy")
        .expect("migration retry");
    let upgrade = calls.find("phux upgrade").expect("listener restart");
    assert!(
        plain < migrated && migrated < upgrade,
        "pair, then the migration retry, then the restart; calls={calls:?}"
    );
    assert!(
        stderr.contains("token store predates versioning; migrated it")
            && stderr.contains("restarted the server so its listeners re-read"),
        "stderr={stderr}"
    );
    home.assert_token_routed(&home.token_path("remotes", "mini"), "satellites");
}

/// ssh itself failing (exit 255) is reported as that, with the ssh check
/// and the `--ssh-only` escape as the remedies; nothing is registered.
#[test]
fn add_reports_an_unreachable_host() {
    let home = EnrollHome::new();
    let ssh =
        home.install_failing_ssh(255, "ssh: connect to host mini port 22: Connection refused");

    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("cannot reach me@mini over ssh")
            && stderr.contains("Connection refused")
            && stderr.contains("`ssh me@mini`")
            && stderr.contains("phux host add me@mini --ssh-only"),
        "stderr={stderr}"
    );
    assert!(
        !home.dir.path().join("config/phux/config.toml").exists(),
        "nothing is registered on a failed setup"
    );

    // The same failure under --json is the one-line contract document.
    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini", "--json"], &ssh);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert_eq!(stderr.lines().count(), 1, "stderr={stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stderr).expect("stderr is JSON");
    assert_eq!(doc["error"]["code"], "registry");
}

/// A host with no phux on its ssh PATH (exit 127) gets the install
/// one-liner and the `--remote-phux` escape; nothing is registered.
#[test]
fn add_reports_a_host_without_phux() {
    let home = EnrollHome::new();
    let ssh = home.install_failing_ssh(127, "sh: phux: command not found");

    let (code, _stdout, stderr) = home.run(&["host", "add", "me@mini"], &ssh);
    assert_eq!(code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("me@mini has no phux")
            && stderr.contains("curl -fsSL https://phux.sh/install | sh")
            && stderr.contains("--remote-phux"),
        "stderr={stderr}"
    );
    assert!(
        !home.dir.path().join("config/phux/config.toml").exists(),
        "nothing is registered on a failed setup"
    );
}

/// `--role satellite` flips every role-specific decision at once: the
/// registry table, the token directory, nothing else.
#[test]
fn add_satellite_registers_satellite_registry_and_satellite_token_dir() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(
        &[
            "host",
            "add",
            "edge",
            "--role",
            "satellite",
            "--endpoint",
            "203.0.113.7:8788",
        ],
        &ssh,
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");

    let config = home.config();
    assert!(
        config.contains("[[satellites]]") && !config.contains("[[remote]]"),
        "a satellite enrollment must land in the satellite registry only; \
         config={config}"
    );
    // A satellite has no `direct` to promote, so an unanswered probe leaves
    // an ssh:// route and no credential: the satellite tail is unchanged.
    assert!(
        config.contains("name = \"edge\"") && config.contains("endpoint = \"ssh://edge\""),
        "config={config}"
    );
    assert!(
        !home.token_path("satellites", "edge").exists(),
        "an ssh:// satellite still rides ssh trust: no pairing token"
    );
    assert_token_never_printed(&stdout, &stderr);
    assert!(
        stdout.contains("local hub service installed with --hub"),
        "a missing local unit is written with --hub; stdout={stdout}"
    );
    let unit = std::fs::read_to_string(home.hub_unit_path()).expect("hub unit written");
    if cfg!(target_os = "macos") {
        assert!(
            unit.contains("<string>--hub</string>"),
            "installed unit must run with --hub:\n{unit}"
        );
    } else {
        assert!(
            unit.contains(" --hub") || unit.contains("server --hub"),
            "installed unit must run with --hub:\n{unit}"
        );
    }
}

/// `--ssh-only` registers `ssh://HOST` in the role-correct registry without
/// contacting the host (the missing `$PHUX_SSH` proves it) and without
/// writing any credential.
#[test]
fn ssh_only_registers_ssh_endpoint_without_contacting_the_host() {
    let never_ssh = Path::new("/nonexistent/phux-test-ssh");

    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini", "--ssh-only"], never_ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let config = home.config();
    assert!(
        config.contains("[[remote]]")
            && config.contains("endpoint = \"ssh://me@mini\"")
            && config.contains("ssh = \"me@mini\""),
        "ssh-only default role registers ssh://HOST as a remote; config={config}"
    );
    assert!(
        !home.dir.path().join("state").exists(),
        "an ssh:// entry rides ssh trust: no token, no state dir"
    );
    assert!(
        stdout.contains("nothing on the host was touched"),
        "stdout={stdout}"
    );

    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(
        &["host", "add", "edge", "--role", "satellite", "--ssh-only"],
        never_ssh,
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let config = home.config();
    assert!(
        config.contains("[[satellites]]") && config.contains("ssh://edge"),
        "ssh-only satellite role registers ssh://HOST as a satellite; \
         config={config}"
    );
    assert!(
        !home.token_path("satellites", "edge").exists(),
        "an ssh:// satellite still rides ssh trust: no pairing token"
    );
    assert!(
        home.hub_unit_path().exists(),
        "ssh-only satellite enroll still enables local --hub"
    );
}

/// `phux host enroll` still works as a hidden alias of the ssh form, and
/// `phux machine` is a visible alias of `phux host` — the word people reach
/// for.
#[test]
fn enroll_and_machine_spellings_reach_the_same_verb() {
    let never_ssh = Path::new("/nonexistent/phux-test-ssh");

    let home = EnrollHome::new();
    let (code, _stdout, stderr) = home.run(&["host", "enroll", "me@mini", "--ssh-only"], never_ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stderr.contains("`phux host enroll` is deprecated") && stderr.contains("phux host add"),
        "the alias warns toward its replacement: {stderr}"
    );
    assert!(home.config().contains("endpoint = \"ssh://me@mini\""));

    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(&["machine", "add", "me@mini", "--ssh-only"], never_ssh);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        !stderr.contains("deprecated"),
        "machine is a plain alias, not a deprecated spelling: {stderr}"
    );
    assert!(
        stdout.contains("Registered mini -> ssh://me@mini"),
        "{stdout}"
    );
}

/// A flag from the other form is a usage error naming the form it belongs
/// to, before anything is contacted or written.
#[test]
fn flags_from_the_other_form_are_refused() {
    let never_ssh = Path::new("/nonexistent/phux-test-ssh");
    let home = EnrollHome::new();

    let (code, _stdout, stderr) = home.run(
        &["host", "add", "me@mini", "--cert-fingerprint", FINGERPRINT],
        never_ssh,
    );
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("manual form") && stderr.contains("quic://HOST:PORT"),
        "stderr={stderr}"
    );

    let (code, _stdout, stderr) = home.run(
        &["host", "add", "mini", "ssh://mini", "--no-service"],
        never_ssh,
    );
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("--no-service") && stderr.contains("ssh form"),
        "stderr={stderr}"
    );
    assert!(
        !home.dir.path().join("config/phux/config.toml").exists(),
        "a usage error registers nothing"
    );
}

/// The `--json` success document: the same `schema_version`-1 `"host"`
/// wrapper the manual form emits, with stdout carrying nothing else and no
/// progress narration anywhere.
#[test]
fn add_json_emits_the_documented_host_document() {
    // The ssh-only remote shape: null auth material, null session.
    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(
        &["host", "add", "me@mini", "--ssh-only", "--json"],
        Path::new("/nonexistent/phux-test-ssh"),
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("`host add --json` stdout is one JSON document");
    assert_eq!(doc["schema_version"], 1, "document: {doc}");
    let host = doc["host"].as_object().expect("a `host` object");
    assert_eq!(host["name"], "mini");
    assert_eq!(host["role"], "remote");
    assert_eq!(host["endpoint"], "ssh://me@mini");
    assert_eq!(host["enabled"], serde_json::Value::Null);
    assert_eq!(host["token_file"], serde_json::Value::Null);
    assert_eq!(host["cert_fingerprint"], serde_json::Value::Null);
    assert_eq!(host["session"], serde_json::Value::Null);
    assert_eq!(host["ssh"], "me@mini");
    assert_eq!(host["direct"], serde_json::Value::Null);
    assert_eq!(
        doc.as_object().map(serde_json::Map::len),
        Some(2),
        "exactly the two documented top-level keys; document: {doc}"
    );
    assert_eq!(host.len(), 9, "exactly the nine documented host keys");

    // The full ssh path under --json: progress is suppressed, the document
    // carries the candidate the probe could not reach.
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();
    let (code, stdout, stderr) = home.run(&["host", "add", "me@mini", "--json"], &ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    assert!(
        stderr.trim().is_empty(),
        "--json suppresses progress: {stderr}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON document");
    assert_eq!(doc["host"]["endpoint"], "ssh://me@mini");
    assert_eq!(doc["host"]["direct"], format!("quic://{OVERLAY}:8788"));
    assert_eq!(doc["host"]["cert_fingerprint"], FINGERPRINT);
    assert_token_never_printed(&stdout, &stderr);
}

/// `--role satellite` on a host that already runs a direct-exec unit: the
/// unit gains `--hub` in place and keeps its existing flags (ADR-0083).
#[test]
fn satellite_add_patches_hub_into_an_existing_unit_without_dropping_flags() {
    let home = EnrollHome::new();
    home.write_hub_unit(unit_without_hub());
    let (code, stdout, stderr) = home.run(
        &["host", "add", "edge", "--role", "satellite", "--ssh-only"],
        Path::new("/nonexistent/phux-test-ssh"),
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    assert!(
        stdout.contains("local hub service: --hub added; existing listeners kept"),
        "stdout={stdout}"
    );
    unit_kept_existing_flags(&std::fs::read_to_string(home.hub_unit_path()).expect("unit"));
}

/// A unit that already runs with `--hub` is left alone and reported as
/// such.
#[test]
fn satellite_add_leaves_a_hub_unit_alone() {
    let home = EnrollHome::new();
    home.write_hub_unit(&unit_with_hub());
    let before = std::fs::read_to_string(home.hub_unit_path()).expect("unit");
    let (code, stdout, stderr) = home.run(
        &["host", "add", "edge", "--role", "satellite", "--ssh-only"],
        Path::new("/nonexistent/phux-test-ssh"),
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    assert!(
        stdout.contains("local hub service already runs with --hub"),
        "stdout={stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(home.hub_unit_path()).expect("unit"),
        before,
        "an already-hub unit must not be rewritten"
    );
}
