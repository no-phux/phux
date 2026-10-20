//! `phux usage --publish` against a real server: the snapshot lands as the
//! `phux.usage/v1` Global key and an unchanged run does not rewrite it.
//!
//! Hermetic: a private home with no vendor credentials, so the collector
//! finds no plans and makes no request.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

#[path = "../common/ambient.rs"]
mod common;

use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::hermetic::{hermetic_env, prepare_dirs};

const READY_DEADLINE: Duration = Duration::from_secs(30);

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn env(dir: &Path) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut env = hermetic_env(dir);
    env.push(("HOME", dir.join("home")));
    env
}

fn phux(dir: &Path, args: &[&str]) -> Output {
    common::phux_cmd(crate::runner::phux_bin())
        .envs(env(dir))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run phux")
}

fn read_key(socket: &Path) -> Option<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut conn = phux_client::attach::connection::Connection::connect(socket)
            .await
            .expect("connect");
        phux_client::global_metadata::read(&mut conn, &["phux.usage/v1"])
            .await
            .expect("read")
            .pop()
            .flatten()
    })
}

#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn publish_writes_the_usage_key_once_per_change() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 path");
    let _server = Server(
        common::phux_cmd(crate::runner::phux_bin())
            .envs(env(dir.path()))
            .args(["server", "--socket", socket_arg, "--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server"),
    );

    let start = Instant::now();
    let out = loop {
        let out = phux(
            dir.path(),
            &["--socket", socket_arg, "usage", "--publish", "--json"],
        );
        if out.status.success() {
            break out;
        }
        assert!(
            start.elapsed() < READY_DEADLINE,
            "publish never succeeded: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let printed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(printed["schema"], "phux.usage/v1", "{printed}");
    assert_eq!(
        printed["plans"],
        serde_json::json!([]),
        "no credentials, no plans"
    );

    let stored: serde_json::Value =
        serde_json::from_slice(&read_key(&socket).expect("the key is set")).expect("JSON");
    assert_eq!(stored, printed, "the server holds exactly what was printed");

    // Nothing changed: the stored document keeps its observation time.
    let again = phux(
        dir.path(),
        &["--socket", socket_arg, "usage", "--publish", "--json"],
    );
    assert!(again.status.success());
    let kept: serde_json::Value = serde_json::from_slice(&read_key(&socket).unwrap()).unwrap();
    assert_eq!(kept["observed_at_ms"], stored["observed_at_ms"]);

    // `--refresh` always rewrites.
    let forced = phux(
        dir.path(),
        &[
            "--socket",
            socket_arg,
            "usage",
            "--publish",
            "--refresh",
            "--json",
        ],
    );
    assert!(forced.status.success());
    let rewritten: serde_json::Value = serde_json::from_slice(&read_key(&socket).unwrap()).unwrap();
    let forced: serde_json::Value = serde_json::from_slice(&forced.stdout).unwrap();
    assert_eq!(rewritten, forced);

    // Without a server, publishing fails and says so.
    let lonely = phux(
        dir.path(),
        &["--socket", "/nonexistent/phux.sock", "usage", "--publish"],
    );
    assert!(!lonely.status.success());
}
