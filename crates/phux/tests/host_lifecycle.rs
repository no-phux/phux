//! Satellite-registry lifecycle through the visible `phux host` verbs
//! (ADR-0066). Formerly `satellite_lifecycle.rs`, driving the deprecated
//! `phux satellite` spellings; the alias behavior itself is pinned in
//! `deprecated_aliases.rs`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::process::Command;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
/// The stderr build banner, now a plain `phux <version>` line, matched in
/// full so the absence assertions below stay meaningful.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

fn run_with_xdg(args: &[&str], xdg_config_home: &std::path::Path) -> (i32, String, String) {
    let out = Command::new(PHUX)
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn add_list_update_remove_json_is_machine_readable() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "devbox",
            "ssh://devbox",
            "--json",
        ],
        &xdg,
    );
    assert_eq!(code, 0, "`host add --json` should exit 0; stderr={stderr}");
    assert!(
        !stdout.contains(BANNER_FRAGMENT) && !stderr.contains(BANNER_FRAGMENT),
        "`host add --json` must be banner-free; stdout={stdout:?} stderr={stderr:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("add stdout is JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["host"]["name"], "devbox");
    assert_eq!(value["host"]["role"], "satellite");
    assert_eq!(value["host"]["endpoint"], "ssh://devbox");
    assert_eq!(value["host"]["enabled"], true);

    let (code, stdout, stderr) =
        run_with_xdg(&["host", "ls", "--role", "satellite", "--json"], &xdg);
    assert_eq!(code, 0, "`host ls --json` should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("ls stdout is JSON");
    assert_eq!(value["hosts"][0]["name"], "devbox");
    assert_eq!(value["hosts"][0]["role"], "satellite");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "devbox",
            "quic://devbox.example:8788",
            "--disabled",
            "--json",
        ],
        &xdg,
    );
    assert_eq!(
        code, 0,
        "`host add` should update existing entries; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("update stdout is JSON");
    assert_eq!(value["host"]["endpoint"], "quic://devbox.example:8788");
    assert_eq!(value["host"]["enabled"], false);

    let config = std::fs::read_to_string(xdg.join("phux").join("config.toml"))
        .expect("read config after update");
    assert_eq!(config.matches("[[satellites]]").count(), 1);
    assert!(config.contains(r#"endpoint = "quic://devbox.example:8788""#));
    assert!(config.contains("enabled = false"));

    let (code, stdout, stderr) = run_with_xdg(
        &["host", "rm", "--role", "satellite", "devbox", "--json"],
        &xdg,
    );
    assert_eq!(code, 0, "`host rm --json` should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("rm stdout is JSON");
    assert_eq!(value["removed"]["name"], "devbox");
    assert_eq!(value["removed"]["role"], "satellite");

    let (code, stdout, stderr) =
        run_with_xdg(&["host", "ls", "--role", "satellite", "--json"], &xdg);
    assert_eq!(
        code, 0,
        "`host ls --json` should exit 0 after removal; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("empty ls stdout is JSON");
    assert_eq!(value["hosts"].as_array().expect("hosts").len(), 0);
}

#[test]
fn auth_material_is_stored_by_reference_and_cleared_on_bare_update() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let token_file = tmp.path().join("lab.token");
    let secret = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    std::fs::write(&token_file, format!("{secret}\n")).expect("write token file");
    let token_file_str = token_file.to_str().expect("utf-8 path");
    let fingerprint = ["AB"; 32].join(":");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "lab",
            "quic://lab.example:8788",
            "--token-file",
            token_file_str,
            "--cert-fingerprint",
            &fingerprint,
            "--json",
        ],
        &xdg,
    );
    assert_eq!(
        code, 0,
        "add with auth material should exit 0; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("add stdout is JSON");
    assert_eq!(value["host"]["token_file"], token_file_str);
    assert_eq!(value["host"]["cert_fingerprint"], fingerprint);
    assert!(
        !stdout.contains(secret),
        "the token secret must never be printed: {stdout}"
    );

    // The registry stores the *path*, never the token bytes.
    let config_path = xdg.join("phux").join("config.toml");
    let config = std::fs::read_to_string(&config_path).expect("read config");
    assert!(config.contains(&format!(r#"token-file = "{token_file_str}""#)));
    assert!(config.contains(&format!(r#"cert-fingerprint = "{fingerprint}""#)));
    assert!(
        !config.contains(secret),
        "the token secret must never land in config.toml: {config}"
    );

    // The human table classifies the auth material (`token+pin`) and never
    // reads or prints the token itself.
    let (code, stdout, stderr) = run_with_xdg(&["host", "ls", "--role", "satellite"], &xdg);
    assert_eq!(code, 0, "ls should exit 0; stderr={stderr}");
    assert!(
        stdout.contains("token+pin"),
        "the AUTH column classifies the material: {stdout}"
    );
    assert!(
        !stdout.contains(secret),
        "ls must not read or print the token"
    );

    // `add` replaces the whole entry: omitting the auth flags clears them.
    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "lab",
            "quic://lab.example:8788",
            "--json",
        ],
        &xdg,
    );
    assert_eq!(code, 0, "bare re-add should exit 0; stderr={stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("update stdout is JSON");
    assert_eq!(value["host"]["token_file"], serde_json::Value::Null);
    assert_eq!(value["host"]["cert_fingerprint"], serde_json::Value::Null);
    let config = std::fs::read_to_string(&config_path).expect("read config after bare update");
    assert!(
        !config.contains("token-file") && !config.contains("cert-fingerprint"),
        "a bare update must clear stale auth material: {config}"
    );
}

#[test]
fn relative_token_file_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "lab",
            "quic://lab.example:8788",
            "--token-file",
            "relative/lab.token",
            "--json",
        ],
        tmp.path(),
    );

    assert_ne!(code, 0, "relative token-file should fail");
    assert!(stdout.is_empty());
    assert!(stderr.contains("token-file must be an absolute path"));
}

#[test]
fn malformed_cert_fingerprint_is_rejected() {
    let tmp = TempDir::new().expect("tempdir");

    for bad in ["AB:CD", "not-a-fingerprint", ""] {
        let (code, stdout, stderr) = run_with_xdg(
            &[
                "host",
                "add",
                "--role",
                "satellite",
                "lab",
                "quic://lab.example:8788",
                "--cert-fingerprint",
                bad,
                "--json",
            ],
            tmp.path(),
        );

        assert_ne!(code, 0, "fingerprint {bad:?} should fail");
        assert!(stdout.is_empty());
        assert!(
            stderr.contains("cert-fingerprint must be a SHA-256 fingerprint"),
            "unexpected stderr for {bad:?}: {stderr}"
        );
    }
}

#[test]
fn duplicate_configured_satellite_names_are_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[[satellites]]
name = "devbox"
endpoint = "ssh://devbox-a"

[[satellites]]
name = "devbox"
endpoint = "ssh://devbox-b"
"#,
    )
    .expect("write config");

    let (code, stdout, stderr) =
        run_with_xdg(&["host", "ls", "--role", "satellite", "--json"], &xdg);

    assert_ne!(code, 0, "duplicate satellite names should be refused");
    assert!(stdout.is_empty());
    // Under `--json` the failure is one line of the shared error contract
    // (phux-i0e8.8.3): code `registry`, with the duplicate named in the
    // message (JSON-escaped, so the assertion parses rather than greps).
    let doc: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("`--json` failure stderr parses as JSON");
    assert_eq!(doc["error"]["code"], "registry");
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(r#"duplicate satellite name "devbox""#)),
        "the message names the duplicate; got {doc}"
    );

    // The same refusal without `--json` keeps the prose spelling scripts
    // already grep for.
    let (code, stdout, stderr) = run_with_xdg(&["host", "ls", "--role", "satellite"], &xdg);
    assert_ne!(code, 0, "duplicate satellite names should be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains(r#"duplicate satellite name "devbox""#));
}

#[test]
fn invalid_endpoint_fails_without_stdout() {
    let tmp = TempDir::new().expect("tempdir");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "devbox",
            "devbox",
            "--json",
        ],
        tmp.path(),
    );

    assert_ne!(code, 0, "invalid endpoint should fail");
    assert!(stdout.is_empty());
    assert!(stderr.contains("endpoint must be a URI"));
}

#[test]
#[cfg(unix)]
fn lifecycle_refuses_to_overwrite_symlinked_config() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let victim = tmp.path().join("victim.toml");
    std::fs::write(&victim, "do-not-touch").expect("write victim");
    std::os::unix::fs::symlink(&victim, config_dir.join("config.toml")).expect("symlink config");

    let (code, stdout, stderr) = run_with_xdg(
        &[
            "host",
            "add",
            "--role",
            "satellite",
            "devbox",
            "ssh://devbox",
            "--json",
        ],
        &xdg,
    );

    assert_ne!(code, 0, "symlinked config should be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains("must not be a symlink"));
    assert_eq!(
        std::fs::read_to_string(victim).expect("read victim"),
        "do-not-touch"
    );
}
