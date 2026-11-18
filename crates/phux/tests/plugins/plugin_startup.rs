//! `phux plugin log --startup`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use crate::common::{XdgDirs, phux_in};

fn run(xdg: &XdgDirs, args: &[&str]) -> (i32, String, String) {
    phux_in(
        [
            ("XDG_CONFIG_HOME", &xdg.config),
            ("XDG_STATE_HOME", &xdg.state),
        ],
        args,
    )
    .into_parts()
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
