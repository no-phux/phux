//! `phux agent usage --publish` against a real server: the snapshot lands as the
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

/// `phux --socket SOCKET agent usage FLAGS...`.
fn usage(dir: &Path, socket: &str, flags: &[&str]) -> Output {
    let mut args = vec!["--socket", socket, "agent", "usage"];
    args.extend_from_slice(flags);
    phux(dir, &args)
}

fn stored(socket: &Path) -> serde_json::Value {
    serde_json::from_slice(&read_key(socket).expect("the key is set")).expect("JSON")
}

fn start_server(dir: &Path, socket: &str) -> Server {
    Server(
        common::phux_cmd(crate::runner::phux_bin())
            .envs(env(dir))
            .args(["server", "--socket", socket, "--exit-after-idle", "120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server"),
    )
}

#[test]
#[ignore = "spawns a real server; runs in the e2e lane"]
fn publish_writes_the_usage_key_once_per_change() {
    let dir = TempDir::new().expect("tempdir");
    prepare_dirs(dir.path());
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    let socket = dir.path().join("s.sock");
    let socket_arg = socket.to_str().expect("utf-8 path");
    let _server = start_server(dir.path(), socket_arg);

    let start = Instant::now();
    let out = loop {
        let out = usage(dir.path(), socket_arg, &["--publish", "--json"]);
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
    let first = stored(&socket);
    assert_eq!(first, printed, "the server holds exactly what was printed");

    // Inside ten seconds of the last run a hook publish does nothing.
    let again = usage(dir.path(), socket_arg, &["--publish", "--json"]);
    assert!(again.status.success());
    assert!(again.stdout.is_empty(), "skipped: {:?}", again.stdout);
    assert_eq!(stored(&socket)["observed_at_ms"], first["observed_at_ms"]);

    // `--refresh` always rewrites.
    let forced = usage(
        dir.path(),
        socket_arg,
        &["--publish", "--refresh", "--json"],
    );
    assert!(forced.status.success());
    let forced: serde_json::Value = serde_json::from_slice(&forced.stdout).unwrap();
    assert_eq!(stored(&socket), forced);

    held_lock_skips_the_publish(dir.path(), &socket, &forced);

    // A plain publish inside the interval is a quiet no-op even with no
    // server; a forced one reaches for the server and fails, saying so.
    let quiet = usage(dir.path(), "/nonexistent/phux.sock", &["--publish"]);
    assert!(quiet.status.success(), "skipped before any connect");
    let lonely = usage(
        dir.path(),
        "/nonexistent/phux.sock",
        &["--publish", "--refresh"],
    );
    assert!(!lonely.status.success());
}

/// Another publish holds the state lock: this one steps aside at once,
/// touching neither the server nor any credential, even with `--refresh`.
fn held_lock_skips_the_publish(dir: &Path, socket: &Path, current: &serde_json::Value) {
    let lock_path = std::fs::read_dir(dir.join("state"))
        .unwrap()
        .map(|entry| entry.unwrap().path().join("usage/lock"))
        .find(|path| path.is_file())
        .expect("the publish created its state lock");
    let held = std::fs::File::options()
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.lock().unwrap();
    let socket_arg = socket.to_str().expect("utf-8 path");
    let blocked = usage(dir, socket_arg, &["--publish", "--refresh", "--json"]);
    assert!(blocked.status.success(), "a held lock is not an error");
    assert!(blocked.stdout.is_empty(), "it published nothing");
    assert_eq!(&stored(socket), current);
}
