//! The agent-stats plugin against a real server: its hooks count an agent's
//! finished task and working time into `phux.agent-stats/v1`, and forget
//! the pane when it exits.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;

use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use crate::hermetic::{hermetic_env, prepare_dirs};

const DEADLINE: Duration = Duration::from_secs(30);

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn phux(dir: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(dir))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

fn stored(socket: &Path) -> Option<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bytes = runtime.block_on(async {
        let mut conn = phux_client::attach::connection::Connection::connect(socket)
            .await
            .ok()?;
        phux_client::global_metadata::read(&mut conn, &["phux.agent-stats/v1"])
            .await
            .ok()?
            .pop()
            .flatten()
    })?;
    Some(serde_json::from_slice(&bytes).expect("JSON"))
}

/// Poll until `ready` holds, panicking with `what` at the deadline.
fn until<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(start.elapsed() < DEADLINE, "never: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn plugin_hooks_count_work_and_prune_exited_panes() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../plugins/agent-stats/phux-plugin.toml")
    .canonicalize()
    .unwrap();
    std::fs::write(
        dir.path().join("config/phux/config.toml"),
        format!(
            "plugins = [{{ manifest = {:?} }}]\n",
            manifest.display().to_string()
        ),
    )
    .unwrap();
    let socket = dir.path().join("s.sock");
    let sock = socket.to_str().expect("utf-8 path");
    let _server = Server(
        common::phux_cmd(crate::runner::phux_bin())
            .envs(hermetic_env(dir.path()))
            .env("PHUX_BIN", crate::runner::phux_bin())
            .args(["server", "--socket", sock, "--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server"),
    );
    let created: Value = until("a pane", || {
        let out = phux(
            dir.path(),
            &[
                "--socket", sock, "new", "--json", "-s", "a", "--", "/bin/sh", "-c", "sleep 6",
            ],
        );
        out.status
            .success()
            .then(|| serde_json::from_slice(&out.stdout).unwrap())
    });
    let id = created["terminal_id"]
        .as_u64()
        .expect("terminal id")
        .to_string();
    let pane = format!("@{id}");
    for state in ["working", "done"] {
        let out = phux(
            dir.path(),
            &[
                "--socket", sock, "agent", "set", "--name", "fox", "--kind", "claude", "--state",
                state, &pane,
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(1100));
    }
    let doc = until("the finished task", || {
        stored(&socket).filter(|doc| doc["agents"][&id]["turns"] == 1)
    });
    assert_eq!(doc["v"], 1, "{doc}");
    let agent = &doc["agents"][&id];
    assert_eq!(
        (agent["kind"].as_str(), agent["name"].as_str()),
        (Some("claude"), Some("fox"))
    );
    assert!(agent["working_s"].as_i64().unwrap() >= 1, "{doc}");
    assert!(agent["first_seen_ms"].as_i64().unwrap() <= agent["updated_ms"].as_i64().unwrap());

    until("the exited pane is forgotten", || {
        stored(&socket).filter(|doc| doc["agents"].get(&id).is_none())
    });
}
