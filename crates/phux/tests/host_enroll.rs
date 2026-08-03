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

    /// Run `phux <args...>` against this home's private config and state,
    /// with `$PHUX_SSH` pointed at `ssh` (a missing path proves the run
    /// never sshed). Returns `(exit_code, stdout, stderr)`.
    fn run(&self, args: &[&str], ssh: &Path) -> (i32, String, String) {
        let out = Command::new(PHUX)
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("PHUX_SSH", ssh)
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
    assert!(!home.dir.path().join("state").exists());
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
}
