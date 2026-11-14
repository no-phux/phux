//! Satellite-registry lifecycle through the visible `phux host` verbs
//! (ADR-0066).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use tempfile::TempDir;

use crate::common::run_with_xdg;

/// The stderr build banner, which no one-shot verb may print.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

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

/// Invalid registry input fails with an empty stdout and a named reason: a
/// relative token file, a malformed fingerprint, a non-URI endpoint.
#[test]
fn invalid_satellite_entries_are_rejected() {
    let tmp = TempDir::new().expect("tempdir");
    let add = ["host", "add", "--role", "satellite", "lab"];
    let quic = "quic://lab.example:8788";
    let cases: &[(&[&str], &str)] = &[
        (
            &[quic, "--token-file", "relative/lab.token"],
            "token-file must be an absolute path",
        ),
        (
            &[quic, "--cert-fingerprint", "AB:CD"],
            "cert-fingerprint must be a SHA-256 fingerprint",
        ),
        (
            &[quic, "--cert-fingerprint", "not-a-fingerprint"],
            "cert-fingerprint must be a SHA-256 fingerprint",
        ),
        (
            &[quic, "--cert-fingerprint", ""],
            "cert-fingerprint must be a SHA-256 fingerprint",
        ),
        (&["lab"], "endpoint must be a URI"),
    ];
    for (extra, reason) in cases {
        let mut args = add.to_vec();
        args.extend_from_slice(extra);
        args.push("--json");
        let (code, stdout, stderr) = run_with_xdg(&args, tmp.path());
        assert_ne!(code, 0, "{args:?} should fail");
        assert!(stdout.is_empty(), "{args:?}");
        assert!(stderr.contains(reason), "{args:?}: {stderr}");
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
    // One contract line, code `registry`, naming the duplicate.
    let doc: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("`--json` failure stderr parses as JSON");
    assert_eq!(doc["error"]["code"], "registry");
    assert!(
        doc["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(r#"duplicate satellite name "devbox""#)),
        "the message names the duplicate; got {doc}"
    );

    let (code, stdout, stderr) = run_with_xdg(&["host", "ls", "--role", "satellite"], &xdg);
    assert_ne!(code, 0, "duplicate satellite names should be refused");
    assert!(stdout.is_empty());
    assert!(stderr.contains(r#"duplicate satellite name "devbox""#));
}

#[test]
#[cfg(unix)]
fn lifecycle_writes_through_a_symlinked_config() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let config_dir = xdg.join("phux");
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    let real = tmp.path().join("dotfiles").join("phux.toml");
    std::fs::create_dir_all(real.parent().unwrap()).expect("dotfiles dir");
    std::fs::write(&real, "# keep\n").expect("write target");
    let link = config_dir.join("config.toml");
    std::os::unix::fs::symlink(&real, &link).expect("symlink config");

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

    assert_eq!(
        code, 0,
        "symlinked config should be written; stderr={stderr}"
    );
    assert!(stdout.contains("devbox"), "stdout={stdout}");
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("link meta")
            .file_type()
            .is_symlink(),
        "the config path must stay a symlink"
    );
    let target = std::fs::read_to_string(&real).expect("read target");
    assert!(target.contains("# keep"), "comment must survive");
    assert!(target.contains("devbox"));
    assert!(target.contains("ssh://devbox"));
}

#[test]
fn host_color_overrides_the_hash_and_ls_reports_it() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let (code, _, stderr) = run_with_xdg(&["host", "add", "mini", "ssh://mini"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    let color = |args: &[&str]| {
        let (code, stdout, stderr) = run_with_xdg(args, &xdg);
        assert_eq!(code, 0, "{args:?}: stderr={stderr}");
        serde_json::from_str::<serde_json::Value>(&stdout).expect("JSON")
    };
    let ls = color(&["host", "ls", "--json"]);
    let auto = serde_json::json!({"id": "magenta", "hex": "#D9629F", "override": false});
    assert_eq!(ls["hosts"][0]["color"], auto);
    assert!(ls["local"]["name"].is_string());
    assert!(ls["local"].get("cert_fingerprint").is_some());

    let set = color(&["host", "color", "mini", "violet", "--json"]);
    assert_eq!(set["host"]["color"]["id"], "violet");
    color(&["host", "color", "--local", "copper", "--json"]);
    let ls = color(&["host", "ls", "--json"]);
    assert_eq!(ls["hosts"][0]["color"]["override"], true);
    assert_eq!(ls["local"]["color"]["hex"], "#C9926B");

    for (id, hex) in [("green", "#3DAA5C"), ("navy", "#2F55C4")] {
        let set = color(&["host", "color", "mini", id, "--json"]);
        assert_eq!(set["host"]["color"]["id"], id);
        let ls = color(&["host", "ls", "--json"]);
        let want = serde_json::json!({"id": id, "hex": hex, "override": true});
        assert_eq!(ls["hosts"][0]["color"], want);
    }
    color(&["host", "color", "mini", "auto", "--json"]);
    let ls = color(&["host", "ls", "--json"]);
    assert_eq!(ls["hosts"][0]["color"], auto);

    let (code, _, _) = run_with_xdg(&["host", "color", "mini", "teal"], &xdg);
    assert_eq!(code, 2, "an unknown colour is a usage error");
}

/// A name registered as both a remote and a satellite: `--role` picks the
/// entry, and an override on one role colours the name in both until the
/// other role sets its own.
#[test]
fn host_color_role_picks_the_registry_and_one_override_colours_both() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    for args in [
        &["host", "add", "ph", "ssh://ph"][..],
        &["host", "add", "--role", "satellite", "ph", "ssh://ph"],
    ] {
        let (code, _, stderr) = run_with_xdg(args, &xdg);
        assert_eq!(code, 0, "{args:?}: stderr={stderr}");
    }
    let run = |args: &[&str]| {
        let (code, stdout, stderr) = run_with_xdg(args, &xdg);
        assert_eq!(code, 0, "{args:?}: stderr={stderr}");
        serde_json::from_str::<serde_json::Value>(&stdout).expect("JSON")
    };
    let ids = || {
        let ls = run(&["host", "ls", "--json"]);
        let hosts = ls["hosts"].as_array().expect("hosts").clone();
        hosts
            .iter()
            .map(|h| {
                (
                    h["role"].as_str().unwrap().to_owned(),
                    h["color"]["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let pair = |remote: &str, satellite: &str| {
        vec![
            ("remote".to_owned(), remote.to_owned()),
            ("satellite".to_owned(), satellite.to_owned()),
        ]
    };

    let (code, _, stderr) = run_with_xdg(&["host", "color", "ph", "navy"], &xdg);
    assert_eq!(code, 2, "an ambiguous name needs --role; stderr={stderr}");
    assert!(stderr.contains("--role satellite ph COLOR"), "{stderr}");

    let set = run(&[
        "host",
        "color",
        "--role",
        "satellite",
        "ph",
        "navy",
        "--json",
    ]);
    assert_eq!(set["host"]["color"]["id"], "navy");
    assert_eq!(
        ids(),
        pair("navy", "navy"),
        "the satellite's override colours the remote"
    );
    let remote_only = run(&["host", "ls", "--role", "remote", "--json"]);
    assert_eq!(remote_only["hosts"][0]["color"]["id"], "navy");
    assert_eq!(remote_only["hosts"][0]["color"]["override"], true);

    run(&["host", "color", "--role", "remote", "ph", "green", "--json"]);
    assert_eq!(
        ids(),
        pair("green", "navy"),
        "each role keeps its own override"
    );

    let cleared = run(&["host", "color", "--role", "remote", "ph", "auto", "--json"]);
    assert_eq!(
        cleared["host"]["color"]["id"], "navy",
        "clearing falls back to the other role"
    );
    run(&[
        "host",
        "color",
        "--role",
        "satellite",
        "ph",
        "auto",
        "--json",
    ]);
    let auto = phux_config::host_color::resolve("ph", None).id;
    assert_eq!(ids(), pair(&auto, &auto));

    let (code, _, _) = run_with_xdg(
        &["host", "color", "--local", "--role", "remote", "sky"],
        &xdg,
    );
    assert_eq!(code, 2, "--local takes no --role");
}
