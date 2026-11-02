//! `phux plugin validate` and `phux plugin log --startup`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::fmt::Write as _;
use std::path::Path;

use crate::common::{XdgDirs, phux_cmd};
use crate::runner::phux_bin;

fn manifest(dir: &Path, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("phux-plugin.toml");
    std::fs::write(
        &path,
        format!(
            "id = \"example.startup\"\nname = \"Startup\"\nversion = \"0.1.0\"\n\
             min_phux_version = \"0.0.2\"\n{body}"
        ),
    )
    .unwrap();
    path
}

fn run(xdg: &XdgDirs, args: &[&str]) -> (i32, String, String) {
    let out = phux_cmd(phux_bin())
        .env("XDG_CONFIG_HOME", &xdg.config)
        .env("XDG_STATE_HOME", &xdg.state)
        .args(args)
        .output()
        .expect("run phux");
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn validate_rejects_a_startup_table_past_the_cap_and_accepts_eight() {
    let tmp = tempfile::tempdir().unwrap();
    let xdg = XdgDirs::fresh("startup-validate");
    let mut body = String::new();
    for n in 0..8 {
        let _ = write!(body, "[[startup]]\nid = \"job{n}\"\ncommand = [\"true\"]\n");
    }
    let path = manifest(tmp.path(), &body);
    let (code, stdout, stderr) = run(&xdg, &["plugin", "validate", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("valid"), "{stdout}");

    body.push_str("[[startup]]\nid = \"job8\"\ncommand = [\"true\"]\n");
    let path = manifest(tmp.path(), &body);
    let (code, _stdout, stderr) = run(&xdg, &["plugin", "validate", path.to_str().unwrap()]);
    assert_ne!(code, 0);
    assert!(stderr.contains("at most 8"), "{stderr}");
}

#[test]
fn plugin_log_startup_prints_the_sidecar_tail() {
    let xdg = XdgDirs::fresh("startup-log");
    // A `cargo test` binary is a dev build, so the profile directory is
    // `phux-dev`, not `phux`.
    let state = xdg.state.join("phux-dev");
    let dir = state.join("plugin-startup").join("example.startup");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("web.log"), "hello from sidecar\nline two\n").unwrap();
    let (code, stdout, stderr) = run(&xdg, &["plugin", "log", "--startup", "example.startup/web"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("hello from sidecar"), "{stdout}");
    assert!(stdout.contains("example.startup/web"), "{stdout}");

    let (code, stdout, stderr) = run(&xdg, &["plugin", "log", "--startup", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(value["logs"][0]["startup_id"], "web");
    assert!(
        value["logs"][0]["tail"]
            .as_str()
            .unwrap()
            .contains("line two")
    );

    std::fs::write(
        state.join("plugin-runs.jsonl"),
        "{\"at_unix_ms\":1,\"kind\":\"startup\",\"plugin_id\":\"example.startup\",\"name\":\"web\",\"command\":[\"true\"],\"outcome\":null,\"exit_code\":null,\"duration_ms\":0,\"stdout\":\"\",\"stderr\":\"\",\"error\":null,\"note\":\"exited 0 (not restarting)\"}\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run(&xdg, &["plugin", "log"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("exited 0 (not restarting)"), "{stdout}");
    assert!(stdout.contains("startup"), "{stdout}");
}
