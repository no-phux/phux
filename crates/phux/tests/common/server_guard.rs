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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::{POLL, SERVER_DEADLINE, SERVER_IDLE_LIMIT_SECS, ServerProcess};

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
    /// The server's private `XDG_STATE_HOME` and `XDG_CONFIG_HOME`, which
    /// [`Self::cmd`] and [`Self::cmd_global`] reuse so a client verb sees the
    /// same state and config as the server it dials.
    pub xdg: super::ambient::XdgDirs,
    /// Instant the child was spawned, matching the server's idle-clock origin.
    pub spawned_at: Instant,
    /// The private home [`ServerSpawn::isolated_home`] made, if any.
    home: Option<IsolatedHome>,
}

/// `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR` under one
/// tempdir, removed when the [`ServerGuard`] that holds it drops.
#[derive(Debug)]
pub struct IsolatedHome {
    dir: tempfile::TempDir,
}

impl IsolatedHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("isolated home tempdir");
        for name in ["home", "config", "state", "runtime"] {
            std::fs::create_dir(dir.path().join(name)).expect("create isolated home directory");
        }
        Self { dir }
    }

    /// The tempdir holding `home`, `config`, `state` and `runtime`.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The four variables, pointing into [`Self::root`].
    pub fn env(&self) -> Vec<(&'static str, PathBuf)> {
        let root = self.root();
        vec![
            ("HOME", root.join("home")),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_STATE_HOME", root.join("state")),
            ("XDG_RUNTIME_DIR", root.join("runtime")),
        ]
    }

    fn xdg(&self) -> super::ambient::XdgDirs {
        super::ambient::XdgDirs {
            state: self.root().join("state"),
            config: self.root().join("config"),
        }
    }
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

/// A verb run against this server may have started a work-store keeper
/// that watches it (`phux mission`); stop it with the server so it cannot
/// idle on past the test.
impl Drop for ServerGuard {
    fn drop(&mut self) {
        // Test binaries run a dev build, so the store lives under `phux-dev`.
        let pidfile = self.xdg.state.join("phux-dev/work/keeper.pid");
        if let Some(pid) = std::fs::read_to_string(pidfile)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 1)
        {
            let _ = super::signal(pid, libc::SIGKILL);
        }
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

    pub fn pid(&self) -> u32 {
        self.process.id()
    }

    /// The private home this server was started with.
    ///
    /// # Panics
    ///
    /// When the builder did not ask for [`ServerSpawn::isolated_home`].
    pub fn isolated_home(&self) -> &IsolatedHome {
        self.home
            .as_ref()
            .expect("server was started without isolated_home()")
    }

    /// The server's isolated home when it has one, then its XDG pair (which
    /// an explicit builder `env` may have moved).
    fn apply_env(&self, cmd: &mut Command) {
        if let Some(home) = &self.home {
            cmd.envs(home.env());
        }
        self.xdg.apply(cmd);
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
        let mut cmd = super::ambient::phux_cmd(crate::runner::phux_bin());
        self.apply_env(&mut cmd);
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
        let mut cmd = super::ambient::phux_cmd(crate::runner::phux_bin());
        self.apply_env(&mut cmd);
        cmd.arg("--socket")
            .arg(&self.socket)
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        super::Out::of(&mut self.cmd(args)).into_parts()
    }

    /// Stdout of a verb that must exit 0, parsed as one JSON document.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        super::Out::of(&mut self.cmd(args)).json()
    }

    pub fn success(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(code, 0, "phux {args:?} exited {code}; stderr={stderr}");
        stdout
    }

    /// Create a pane running `command` and return its local Terminal id.
    /// `--socket` precedes the verb so `spawn`'s trailing argv cannot take it.
    pub fn spawn_pane(&self, command: &std::path::Path) -> u32 {
        let out = self
            .cmd_global(&["spawn", "--json", "--"])
            .arg(command)
            .output()
            .expect("run phux spawn");
        assert!(
            out.status.success(),
            "phux spawn exited {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("phux spawn --json");
        json["terminal_id"]
            .as_u64()
            .expect("spawn reports a terminal id")
            .try_into()
            .expect("terminal id fits u32")
    }
}

/// Builder for a [`ServerGuard`]. Every field has a default that matches the
/// duplicated harnesses: session `work`, idle backstop
/// [`SERVER_IDLE_LIMIT_SECS`], no seed command, inherited environment. The
/// server always gets a fresh `XDG_STATE_HOME` and `XDG_CONFIG_HOME`; an
/// explicit [`Self::env`] for either key overrides it.
#[derive(Debug)]
pub struct ServerSpawn {
    prefix: String,
    session: String,
    seed_command: Option<String>,
    idle_secs: String,
    env_clear: bool,
    extra_env: Vec<(OsString, OsString)>,
    home: Option<IsolatedHome>,
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
            home: None,
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

    /// Give the server, and every [`ServerGuard::cmd`], a private `HOME`,
    /// XDG config, state and runtime dir under one tempdir the guard holds.
    /// An explicit [`Self::env`] for any of those keys still wins.
    #[must_use]
    pub fn isolated_home(mut self) -> Self {
        self.home = Some(IsolatedHome::new());
        self
    }

    /// The private home [`Self::isolated_home`] made, for preparing config
    /// before [`Self::start`].
    ///
    /// # Panics
    ///
    /// When [`Self::isolated_home`] was not called.
    pub fn home(&self) -> &IsolatedHome {
        self.home.as_ref().expect("isolated_home() before home()")
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
        let (mut cmd, socket, xdg, home) = self.into_command(configure);
        let child = cmd.spawn().expect("spawn phux server");
        let spawned_at = Instant::now();
        let guard = ServerGuard {
            process: ServerProcess::from_child(child, socket.clone()),
            socket,
            xdg,
            spawned_at,
            home,
        };
        wait_for_socket(&guard.socket);
        guard
    }

    /// The `phux server` command [`Self::start_with`] spawns, before the child
    /// exists. The release-check opt-out is applied after `configure`.
    pub(crate) fn into_command(
        self,
        configure: impl FnOnce(&mut Command),
    ) -> (
        Command,
        PathBuf,
        super::ambient::XdgDirs,
        Option<IsolatedHome>,
    ) {
        let socket = unique_socket(&self.prefix);
        let mut xdg = self.home.as_ref().map_or_else(
            || super::ambient::XdgDirs::fresh(&self.prefix),
            IsolatedHome::xdg,
        );
        let mut cmd = super::ambient::phux_cmd(crate::runner::phux_bin());
        if self.env_clear {
            cmd.env_clear();
        }
        xdg.apply(&mut cmd);
        if let Some(home) = &self.home {
            cmd.envs(home.env());
        }
        for (key, value) in &self.extra_env {
            cmd.env(key, value);
            if key == "XDG_STATE_HOME" {
                xdg.state = PathBuf::from(value);
            } else if key == "XDG_CONFIG_HOME" {
                xdg.config = PathBuf::from(value);
            }
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
        // After configure: `env_clear` on the builder or in the closure must
        // not reopen the release check for this process or a client it spawns.
        super::ambient::disable_update_check(&mut cmd);
        (cmd, socket, xdg, self.home)
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
