//! Satellite-registry lifecycle through the visible `phux host` verbs
//! (ADR-0066).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use tempfile::TempDir;

use crate::common::run_with_xdg;

/// The stderr build banner, which no one-shot verb may print.
const BANNER_FRAGMENT: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

/// `host add --role satellite REST...`.
fn satellite_add<'a>(rest: &[&'a str]) -> Vec<&'a str> {
    [&["host", "add", "--role", "satellite"][..], rest].concat()
}

/// Run `args`, assert exit 0, and parse stdout as JSON.
fn json_ok(args: &[&str], xdg: &std::path::Path) -> serde_json::Value {
    let (code, stdout, stderr) = run_with_xdg(args, xdg);
    assert_eq!(code, 0, "{args:?}: stderr={stderr}");
    serde_json::from_str(&stdout).expect("stdout is JSON")
}

#[test]
fn add_list_update_remove_json_is_machine_readable() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");

    let (code, stdout, stderr) =
        run_with_xdg(&satellite_add(&["devbox", "ssh://devbox", "--json"]), &xdg);
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

    let value = json_ok(&["host", "ls", "--role", "satellite", "--json"], &xdg);
    assert_eq!(value["hosts"][0]["name"], "devbox");
    assert_eq!(value["hosts"][0]["role"], "satellite");

    let (code, stdout, stderr) = run_with_xdg(
        &satellite_add(&[
            "devbox",
            "quic://devbox.example:8788",
            "--disabled",
            "--json",
        ]),
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

    let value = json_ok(
        &["host", "rm", "--role", "satellite", "devbox", "--json"],
        &xdg,
    );
    assert_eq!(value["removed"]["name"], "devbox");
    assert_eq!(value["removed"]["role"], "satellite");

    let value = json_ok(&["host", "ls", "--role", "satellite", "--json"], &xdg);
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
        &satellite_add(&[
            "lab",
            "quic://lab.example:8788",
            "--token-file",
            token_file_str,
            "--cert-fingerprint",
            &fingerprint,
            "--json",
        ]),
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
        &satellite_add(&["lab", "quic://lab.example:8788", "--json"]),
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
fn host_color_overrides_the_hash_and_ls_reports_it() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let (code, _, stderr) = run_with_xdg(&["host", "add", "mini", "ssh://mini"], &xdg);
    assert_eq!(code, 0, "stderr={stderr}");
    let color = |args: &[&str]| json_ok(args, &xdg);
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

    color(&["host", "color", "mini", "auto", "--json"]);
    let ls = color(&["host", "ls", "--json"]);
    assert_eq!(ls["hosts"][0]["color"], auto);

    let (code, _, _) = run_with_xdg(&["host", "color", "mini", "teal"], &xdg);
    assert_eq!(code, 2, "an unknown colour is a usage error");
}

#[test]
fn a_second_server_on_one_machine_is_saved_beside_the_first() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let pin = "ab".repeat(32);
    let add = |endpoint: &str| {
        let args = [
            "host",
            "add",
            endpoint,
            "--cert-fingerprint",
            &pin,
            "--json",
        ];
        json_ok(&args, &xdg)["host"]["name"]
            .as_str()
            .expect("name")
            .to_owned()
    };
    assert_eq!(add("quic://127.0.0.1:8788"), "127.0.0.1");
    assert_eq!(add("quic://127.0.0.1:9788"), "127.0.0.1-9788");
    // Re-adding either one updates it in place.
    assert_eq!(add("quic://127.0.0.1:9788"), "127.0.0.1-9788");
    assert_eq!(add("quic://127.0.0.1:8788"), "127.0.0.1");
    let value = json_ok(&["host", "ls", "--json"], &xdg);
    let hosts = value["hosts"].as_array().expect("hosts");
    assert_eq!(hosts.len(), 2, "{value}");
}

/// An `ssh://` entry carries only an attach, so every command `host add` and
/// a refused session verb print is one that works, for any spelling `ssh`
/// itself takes.
#[test]
fn an_ssh_entry_names_only_commands_that_work() {
    for (endpoint, destination) in [
        ("ssh://me@mini", "me@mini"),
        ("ssh://me@mini:2222", "ssh://me@mini:2222"),
        ("ssh://me@[::1]", "me@::1"),
    ] {
        let tmp = TempDir::new().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let (code, stdout, stderr) = run_with_xdg(&["host", "add", "box", endpoint], &xdg);
        assert_eq!(code, 0, "{stderr}");
        assert!(
            stdout.contains("phux attach box")
                && stdout.contains(&format!("ssh {destination} phux ls"))
                && !stdout.contains("--remote box"),
            "{stdout}"
        );
        let (code, _, stderr) = run_with_xdg(&["ls", "--remote", "box"], &xdg);
        assert_eq!(code, 1, "{stderr}");
        assert!(
            stderr.contains(&format!("ssh {destination} phux ls"))
                && !stderr.contains("phux host add box"),
            "{stderr}"
        );
    }
    let tmp = TempDir::new().expect("tempdir");
    let xdg = tmp.path().join("xdg");
    let (code, _, stderr) = run_with_xdg(&["host", "add", "box", "ssh://mini/home"], &xdg);
    assert_ne!(code, 0);
    assert!(stderr.contains("takes no path"), "{stderr}");
}
