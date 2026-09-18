//! Shared spawn+wait-for-socket harness for binary e2e tests (phux-n0du).
//!
//! Each suite used to copy the same `phux server` child, unique UDS path,
//! idle-backstop argv, and poll-until-the-socket-appears loop. Drop still
//! kills through [`super::ServerProcess`]: an assertion failure must not
//! leak a daemon.

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(clippy::expect_used, reason = "test harness")]
#![allow(clippy::unwrap_used, reason = "test harness")]
#![allow(clippy::panic, reason = "test harness")]
#![allow(
    clippy::missing_const_for_fn,
    reason = "test helper API favors uniform lifecycle constructors"
)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::{POLL, SERVER_DEADLINE, SERVER_IDLE_LIMIT_SECS, ServerProcess};

/// Path to the freshly-built `phux` binary under test.
pub const PHUX: &str = env!("CARGO_BIN_EXE_phux");

/// Monotonic counter so two suites in one process never collide on a path.
static SOCKET_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A running `phux server` child on a private UDS, killed and unlinked on drop.
///
/// Socket paths live at the root of `/tmp` so macOS's 104-byte `sun_path`
/// cap is never in play even when tests run from a deep worktree. The path is
/// prefix-, pid-, and counter-qualified, and unlinked by [`ServerProcess`].
pub struct ServerGuard {
    process: ServerProcess,
    pub socket: PathBuf,
    /// Instant the child was spawned, matching the server's idle-clock origin.
    pub spawned_at: Instant,
}

impl fmt::Debug for ServerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerGuard")
            .field("socket", &self.socket)
            .field("pid", &self.pid())
            .field("spawned_at", &self.spawned_at)
            .finish_non_exhaustive()
    }
}

impl ServerGuard {
    /// Spawn `phux server --session work` with the idle backstop and wait
    /// until the socket file appears.
    #[must_use]
    pub fn start(prefix: &str) -> Self {
        ServerSpawn::new(prefix).start()
    }

    #[must_use]
    pub fn builder(prefix: &str) -> ServerSpawn {
        ServerSpawn::new(prefix)
    }

    pub fn process_mut(&mut self) -> &mut ServerProcess {
        &mut self.process
    }

    pub fn pid(&self) -> u32 {
        self.process.id()
    }

    pub fn sigkill(&mut self) {
        self.process.sigkill();
    }

    pub fn wait_for_exit(&mut self, within: Duration) -> Option<Duration> {
        self.process.wait_for_exit(within)
    }

    #[must_use]
    pub fn socket_text(&self) -> String {
        self.socket.to_string_lossy().into_owned()
    }

    /// `phux <verb> --socket <sock> <rest...>`. `--socket` is injected
    /// immediately after the verb so `trailing_var_arg` verbs cannot swallow it.
    #[must_use]
    pub fn cmd(&self, args: &[&str]) -> Command {
        let (verb, rest) = args.split_first().expect("at least a verb");
        let mut cmd = Command::new(PHUX);
        cmd.arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null());
        cmd
    }

    /// `phux --socket <sock> <args...>`. Root-global form (ADR-0065).
    #[must_use]
    pub fn cmd_global(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(PHUX);
        cmd.arg("--socket")
            .arg(&self.socket)
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.cmd(args).output().expect("run phux verb");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    pub fn success(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(code, 0, "phux {args:?} exited {code}; stderr={stderr}");
        stdout
    }
}

/// Builder for a [`ServerGuard`]. Every field has a default that matches the
/// duplicated harnesses: session `work`, idle backstop
/// [`SERVER_IDLE_LIMIT_SECS`], no seed command, inherited environment.
#[derive(Debug)]
pub struct ServerSpawn {
    prefix: String,
    session: String,
    seed_command: Option<String>,
    idle_secs: String,
    env_clear: bool,
    extra_env: Vec<(OsString, OsString)>,
}

impl ServerSpawn {
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            session: String::from("work"),
            seed_command: None,
            idle_secs: SERVER_IDLE_LIMIT_SECS.to_owned(),
            env_clear: false,
            extra_env: Vec::new(),
        }
    }

    #[must_use]
    pub fn session(mut self, session: impl Into<String>) -> Self {
        self.session = session.into();
        self
    }

    #[must_use]
    pub fn seed_command(mut self, command: impl Into<String>) -> Self {
        self.seed_command = Some(command.into());
        self
    }

    #[must_use]
    pub fn idle_secs(mut self, secs: u64) -> Self {
        self.idle_secs = secs.to_string();
        self
    }

    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.extra_env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    #[must_use]
    pub fn envs<K, V>(mut self, envs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.extra_env.extend(
            envs.into_iter()
                .map(|(key, value)| (key.as_ref().to_os_string(), value.as_ref().to_os_string())),
        );
        self
    }

    /// Spawn the server and block until the socket file exists.
    #[must_use]
    pub fn start(self) -> ServerGuard {
        self.start_with(|_| {})
    }

    /// As [`Self::start`], after `configure` has had a chance to mutate the
    /// `Command` (isolation tables, extra flags).
    #[must_use]
    pub fn start_with(self, configure: impl FnOnce(&mut Command)) -> ServerGuard {
        let socket = unique_socket(&self.prefix);
        let mut cmd = Command::new(PHUX);
        if self.env_clear {
            cmd.env_clear();
        }
        for (key, value) in &self.extra_env {
            cmd.env(key, value);
        }
        cmd.args(["server", "--session", &self.session, "--socket"])
            .arg(&socket);
        if let Some(seed) = &self.seed_command {
            cmd.arg("--seed-command").arg(seed);
        }
        cmd.args(["--exit-after-idle", &self.idle_secs])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure(&mut cmd);
        let child = cmd.spawn().expect("spawn phux server");
        let spawned_at = Instant::now();
        let guard = ServerGuard {
            process: ServerProcess::from_child(child, socket.clone()),
            socket,
            spawned_at,
        };
        wait_for_socket(&guard.socket);
        guard
    }
}

fn unique_socket(prefix: &str) -> PathBuf {
    let n = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    let socket = PathBuf::from(format!(
        "/tmp/phux-{prefix}-{}-{n}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    socket
}

fn wait_for_socket(socket: &std::path::Path) {
    let deadline = Instant::now() + SERVER_DEADLINE;
    while Instant::now() < deadline {
        if socket.exists() {
            return;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "phux server did not bind {} within {SERVER_DEADLINE:?}",
        socket.display()
    );
}
