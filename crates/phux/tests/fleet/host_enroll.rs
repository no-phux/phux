//! The role-specific tails of `phux host enroll`, pinned at the binary
//! level (phux-i0e8.12.7).
//!
//! Wave-10 left `finish_enroll` covered only indirectly (parse tests,
//! error-helper units, the wave-.2 registry/row tests). These tests drive
//! the REAL binary through both enrollment tails, network-free:
//!
//!   * the full ssh path runs against a fake `ssh` via `$PHUX_SSH` — the
//!     same seam the federation hub's satellite dialer uses — which answers
//!     `phux --version`, `phux service install`, and `phux pair --json`
//!     from a script;
//!   * `--ssh-only` must never contact the host at all, so its `$PHUX_SSH`
//!     points at a path that does not exist: any ssh attempt fails the run.
//!
//! What they pin: each role registers into ITS registry (`[[remote]]` vs
//! `[[satellites]]` in the one config.toml) with the pairing token under
//! the role-correct state directory (`remotes/` vs `satellites/`);
//! `--ssh-only` registers `ssh://HOST` and leaves no credential behind;
//! and the `--json` success document is the documented `schema_version`-1
//! `"host"` wrapper.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// A 64-hex pairing token for the fake remote to mint.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A well-formed SHA-256 certificate fingerprint.
const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";

/// One scratch home for a single enrollment run: private config, state,
/// and (when the flow is allowed to "ssh") a fake `ssh` answering from a
/// script.
struct EnrollHome {
    dir: TempDir,
}

impl EnrollHome {
    fn new() -> Self {
        Self {
            dir: TempDir::new().expect("tempdir"),
        }
    }

    /// Write the fake `ssh` and return its path. It answers the three
    /// commands `enroll_over_ssh` issues; anything else fails the run.
    fn install_fake_ssh(&self) -> std::path::PathBuf {
        let path = self.dir.path().join("fake-ssh");
        let script = format!(
            "#!/bin/sh\n\
             # argv: -o BatchMode=yes HOST phux <subcommand...>\n\
             case \"$*\" in\n\
               *\"phux --version\"*) echo \"phux 0.0.0-test\" ;;\n\
               *\"phux service install\"*) echo \"service installed\" ;;\n\
               *\"phux pair --json\"*)\n\
                 printf '%s\\n' '{{\"token\":\"{TOKEN}\",\"cert_fingerprint\":\"{FINGERPRINT}\",\"overlay_addresses\":[\"100.64.0.7\"]}}' ;;\n\
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
        let out = Command::new(PHUX)
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("PHUX_PROFILE", "default")
            .env("PHUX_SSH", ssh)
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
/// `remotes/`, and the chosen endpoint the pinned quic address.
#[test]
fn enroll_remote_registers_remote_registry_and_remote_token_dir() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(&["host", "enroll", "me@mini"], &ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");

    let config = home.config();
    assert!(
        config.contains("[[remote]]") && !config.contains("[[satellites]]"),
        "a remote enrollment must land in the remote registry only; config={config}"
    );
    assert!(
        config.contains("name = \"mini\"") && config.contains("quic://100.64.0.7:8788"),
        "the entry carries the default name and the overlay-derived quic \
         endpoint; config={config}"
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
}

/// `--role satellite` flips every role-specific decision at once: the
/// registry table, the token directory, nothing else.
#[test]
fn enroll_satellite_registers_satellite_registry_and_satellite_token_dir() {
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(&["host", "enroll", "edge", "--role", "satellite"], &ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");

    let config = home.config();
    assert!(
        config.contains("[[satellites]]") && !config.contains("[[remote]]"),
        "a satellite enrollment must land in the satellite registry only; \
         config={config}"
    );
    assert!(
        config.contains("name = \"edge\"") && config.contains("quic://100.64.0.7:8788"),
        "config={config}"
    );
    home.assert_token_routed(&home.token_path("satellites", "edge"), "remotes");
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
    let (code, stdout, stderr) = home.run(&["host", "enroll", "me@mini", "--ssh-only"], never_ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let config = home.config();
    assert!(
        config.contains("[[remote]]") && config.contains("ssh://me@mini"),
        "ssh-only default role registers ssh://HOST as a remote; config={config}"
    );
    assert!(
        !home.dir.path().join("state").exists(),
        "an ssh:// entry rides ssh trust: no token, no state dir"
    );

    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(
        &[
            "host",
            "enroll",
            "edge",
            "--role",
            "satellite",
            "--ssh-only",
        ],
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

/// The `--json` success document: the same `schema_version`-1 `"host"`
/// wrapper `host add --json` emits, with stdout carrying nothing else.
#[test]
fn enroll_json_emits_the_documented_host_document() {
    // The ssh-only remote shape: null auth material, null session.
    let home = EnrollHome::new();
    let (code, stdout, stderr) = home.run(
        &["host", "enroll", "me@mini", "--ssh-only", "--json"],
        Path::new("/nonexistent/phux-test-ssh"),
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("`host enroll --json` stdout is one JSON document");
    assert_eq!(doc["schema_version"], 1, "document: {doc}");
    let host = doc["host"].as_object().expect("a `host` object");
    assert_eq!(host["name"], "mini");
    assert_eq!(host["role"], "remote");
    assert_eq!(host["endpoint"], "ssh://me@mini");
    assert_eq!(host["enabled"], serde_json::Value::Null);
    assert_eq!(host["token_file"], serde_json::Value::Null);
    assert_eq!(host["cert_fingerprint"], serde_json::Value::Null);
    assert_eq!(host["session"], serde_json::Value::Null);
    assert_eq!(
        doc.as_object().map(serde_json::Map::len),
        Some(2),
        "exactly the two documented top-level keys; document: {doc}"
    );
    assert_eq!(host.len(), 7, "exactly the seven documented host keys");
    assert!(
        doc.get("hub_service").is_none(),
        "--role remote JSON must not grow a hub_service key; document: {doc}"
    );

    // The full satellite path fills the auth material in the same shape.
    let home = EnrollHome::new();
    let ssh = home.install_fake_ssh();
    let (code, stdout, stderr) = home.run(
        &["host", "enroll", "edge", "--role", "satellite", "--json"],
        &ssh,
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(doc["schema_version"], 1, "document: {doc}");
    assert_eq!(doc["host"]["name"], "edge");
    assert_eq!(doc["host"]["role"], "satellite");
    assert_eq!(doc["host"]["endpoint"], "quic://100.64.0.7:8788");
    assert_eq!(doc["host"]["enabled"], true);
    assert_eq!(doc["host"]["cert_fingerprint"], FINGERPRINT);
    let token_file = doc["host"]["token_file"]
        .as_str()
        .expect("the token path is machine-readable by reference");
    assert_eq!(
        Path::new(token_file),
        home.token_path("satellites", "edge"),
        "the document names the role-correct token path"
    );
    assert_eq!(
        doc["hub_service"], "installed",
        "no pre-existing unit is written with --hub; document: {doc}"
    );
    assert_token_never_printed(&stdout, &stderr);
}

/// An installed unit that already has listeners must gain `--hub` without
/// losing them. A re-run of `phux service install --hub` would drop both
/// (ADR-0083); this is the whole reason enroll patches in place.
#[test]
fn satellite_enroll_adds_hub_without_dropping_existing_unit_flags() {
    let home = EnrollHome::new();
    home.write_hub_unit(unit_without_hub());
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(&["host", "enroll", "edge", "--role", "satellite"], &ssh);
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    assert!(
        stdout.contains("existing listeners kept"),
        "stdout={stdout}"
    );
    let body = std::fs::read_to_string(home.hub_unit_path()).expect("read unit");
    unit_kept_existing_flags(&body);
    assert_token_never_printed(&stdout, &stderr);
}

/// A unit that already runs with `--hub` is left byte-for-byte alone.
#[test]
fn satellite_enroll_is_a_noop_when_the_local_unit_already_is_a_hub() {
    let home = EnrollHome::new();
    let original = unit_with_hub();
    home.write_hub_unit(&original);
    let ssh = home.install_fake_ssh();

    let (code, stdout, stderr) = home.run(
        &["host", "enroll", "edge", "--role", "satellite", "--json"],
        &ssh,
    );
    assert_eq!(code, 0, "stderr={stderr} stdout={stdout}");
    let doc: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(doc["hub_service"], "already", "document: {doc}");
    let body = std::fs::read_to_string(home.hub_unit_path()).expect("read unit");
    assert_eq!(body, original, "an already-hub unit must not be rewritten");
}
