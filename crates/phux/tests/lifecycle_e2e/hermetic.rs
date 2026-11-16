//! The private environment the remote-dial lifecycle suites run under:
//! their own config, state, and runtime dirs, and no ssh or overlay probe.

#![allow(
    unreachable_pub,
    reason = "shared by sibling suites in this test binary"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use crate::common;

/// How long a freshly started server has to answer its first dial.
pub const READY_DEADLINE: Duration = Duration::from_secs(30);

/// No extra environment for [`phux_with`] and [`await_success`].
pub const NO_ENV: &[(&str, &Path)] = &[];

/// A running `phux server`, killed and reaped on drop.
pub type Server = common::ServerProcess;

/// The environment every process in a hermetic test runs under.
pub fn hermetic_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("run")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        // Any ssh or overlay probe fails loudly instead of reaching out.
        ("PHUX_SSH", dir.join("no-such-ssh")),
        ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
    ]
}

/// Create the config and runtime dirs [`hermetic_env`] points at.
pub fn prepare_dirs(dir: &Path) {
    std::fs::create_dir_all(dir.join("config/phux")).expect("config dir");
    std::fs::create_dir_all(dir.join("run")).expect("runtime dir");
}

/// Run `phux ARGS` in `dir` with `stdin` piped in and `extra` environment.
/// The working directory is the test's own, so a relative path an argument
/// names can never land in the source tree.
pub fn phux_with<V: AsRef<OsStr>>(
    dir: &Path,
    args: &[&str],
    stdin: &[u8],
    extra: &[(&str, V)],
) -> Output {
    let mut child = common::phux_cmd(crate::runner::phux_bin())
        .envs(hermetic_env(dir))
        .envs(extra.iter().map(|(key, value)| (*key, value.as_ref())))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux");
    // A refused invocation may exit before reading; a broken pipe is fine.
    let _ = child.stdin.take().expect("stdin").write_all(stdin);
    child.wait_with_output().expect("wait for phux")
}

pub fn phux(dir: &Path, args: &[&str]) -> Output {
    phux_with(dir, args, b"", NO_ENV)
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn json_doc(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "expected one JSON document ({err}): {}{}",
            text(&out.stdout),
            text(&out.stderr)
        )
    })
}

/// `phux server --socket DIR/s.sock EXTRA...` under [`hermetic_env`].
pub fn server_command(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = common::phux_cmd(crate::runner::phux_bin());
    command
        .envs(hermetic_env(dir))
        .envs(env.iter().copied())
        .current_dir(dir)
        .arg("server")
        .arg("--socket")
        .arg(dir.join("s.sock"))
        .args(extra)
        .stdin(Stdio::null());
    command
}

/// Spawn [`server_command`]. The idle exit is a backstop only: Drop kills the
/// server, but a panic that skips it must not leave a daemon for long.
pub fn start_server(dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Server {
    let child = server_command(dir, extra, env)
        .args(["--exit-after-idle", "120"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn phux server");
    Server::from_child(child, dir.join("s.sock"))
}

/// Run `phux ARGS` until it succeeds, failing after [`READY_DEADLINE`].
pub fn await_success<V: AsRef<OsStr>>(dir: &Path, args: &[&str], extra: &[(&str, V)]) -> Output {
    let mut last = None;
    common::until(READY_DEADLINE, || {
        let out = phux_with(dir, args, b"", extra);
        if out.status.success() {
            return Some(out);
        }
        last = Some(out);
        None
    })
    .unwrap_or_else(|| {
        panic!(
            "`phux {}` never succeeded: {}",
            args.join(" "),
            last.map_or_else(String::new, |out| text(&out.stderr))
        )
    })
}
