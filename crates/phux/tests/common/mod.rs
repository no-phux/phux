#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(
    clippy::missing_const_for_fn,
    reason = "test helper API favors uniform lifecycle constructors"
)]
#![allow(clippy::print_stderr, reason = "Drop cannot return cleanup failures")]

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SERVER_IDLE_LIMIT_SECS: &str = "600";
const SERVER_DEADLINE: Duration = Duration::from_secs(30);
const GRACEFUL_DEADLINE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// Poll `socket` until a connect succeeds (not merely until the file exists:
/// a stale file is what the service suites distinguish), or time out.
pub fn wait_until_accepting(socket: &Path) -> bool {
    let deadline = Instant::now() + SERVER_DEADLINE;
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

mod server_guard;
#[allow(
    unused_imports,
    reason = "re-exported for suites that spawn a ServerGuard; other common consumers do not"
)]
pub use server_guard::ServerGuard;

/// Owns a directly spawned server until it has actually been reaped.
pub struct ServerProcess {
    child: Child,
    socket: PathBuf,
}

impl ServerProcess {
    pub fn from_child(child: Child, socket: PathBuf) -> Self {
        Self { child, socket }
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn sigkill(&mut self) {
        signal(self.child.id(), libc::SIGKILL).expect("SIGKILL the server");
        let _ = self.child.wait();
    }

    pub fn has_exited(&mut self) -> bool {
        self.child
            .try_wait()
            .unwrap_or_else(|err| panic!("try_wait on phux server: {err}"))
            .is_some()
    }

    pub fn wait_for_exit(&mut self, within: Duration) -> Option<Duration> {
        let start = Instant::now();
        while start.elapsed() < within {
            if self.has_exited() {
                return Some(start.elapsed());
            }
            std::thread::sleep(POLL);
        }
        None
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none()
            && let Err(err) = signal(self.child.id(), libc::SIGKILL)
        {
            eprintln!("failed to SIGKILL test server {}: {err}", self.child.id());
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Tracks a daemonized auto-spawn. Drop reaps a live socket even when the
/// PID was never captured (phux-e4qx).
pub struct AutoSpawnedServer {
    phux: PathBuf,
    socket: PathBuf,
    pid: Option<u32>,
}

impl AutoSpawnedServer {
    /// The environment pair that bounds a daemon this type could not reap (a
    /// runner killed outright). Hermetic harnesses `env_clear()` and must re-arm
    /// it from here; the key is the server's own constant.
    pub const IDLE_BACKSTOP: (&'static str, &'static str) =
        (phux::AUTO_SPAWN_IDLE_ENV, SERVER_IDLE_LIMIT_SECS);

    /// Arm a Drop guard for `socket` before the command that may daemonize.
    /// Cleanup does not wait for [`Self::capture_pid`]; a live socket is enough.
    pub fn new(phux: impl Into<PathBuf>, socket: PathBuf) -> Self {
        Self {
            phux: phux.into(),
            socket,
            pid: None,
        }
    }

    /// Capture the daemon PID without invoking any auto-spawning verb.
    pub fn capture_pid(&mut self) -> u32 {
        let deadline = Instant::now() + SERVER_DEADLINE;
        loop {
            if let Some(pid) = pid_from_status(&self.phux, &self.socket) {
                self.pid = Some(pid);
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "could not capture server PID from status --json for {}",
                self.socket.display()
            );
            std::thread::sleep(POLL);
        }
    }

    pub fn cleanup(&self) -> Result<(), String> {
        // Discover the pid while the socket may still answer, then stop.
        // A missing pid used to make this a no-op even when a daemon had
        // already bound `socket` (phux-e4qx).
        let pid = self
            .pid
            .or_else(|| pid_from_status(&self.phux, &self.socket));

        if pid.is_some_and(process_exists) || self.socket.exists() {
            let _ = Command::new(&self.phux)
                .args(["kill", "--server", "--socket"])
                .arg(&self.socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        if let Some(pid) = pid {
            let started = Instant::now();
            let mut term_sent = false;
            let mut kill_sent = false;
            while process_exists(pid) {
                if !term_sent && started.elapsed() >= GRACEFUL_DEADLINE {
                    signal(pid, libc::SIGTERM)?;
                    term_sent = true;
                }
                if !kill_sent && started.elapsed() >= GRACEFUL_DEADLINE * 2 {
                    signal(pid, libc::SIGKILL)?;
                    kill_sent = true;
                }
                if started.elapsed() >= SERVER_DEADLINE {
                    return Err(format!(
                        "server process {pid} did not exit within {SERVER_DEADLINE:?}"
                    ));
                }
                std::thread::sleep(POLL);
            }
        }

        if self.socket.exists() {
            std::fs::remove_file(&self.socket)
                .map_err(|err| format!("remove stale socket {}: {err}", self.socket.display()))?;
        }
        Ok(())
    }
}

impl Drop for AutoSpawnedServer {
    fn drop(&mut self) {
        if let Err(err) = self.cleanup() {
            eprintln!("failed to clean up auto-spawned test server: {err}");
        }
    }
}

fn pid_from_status(phux: &Path, socket: &Path) -> Option<u32> {
    let output = Command::new(phux)
        .args(["status", "--json", "--socket"])
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()?
        .get("pid")?
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
}

pub fn process_exists(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: kill(pid, 0) sends no signal and only queries process existence.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn signal(pid: u32, signal: libc::c_int) -> Result<(), String> {
    let pid = i32::try_from(pid).map_err(|err| format!("invalid server pid {pid}: {err}"))?;
    // SAFETY: the PID was obtained from this test's server over its private
    // socket, and signal is one of SIGTERM/SIGKILL.
    let result = unsafe { libc::kill(pid, signal) };
    let error = std::io::Error::last_os_error();
    if result == 0 || error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!("kill({pid}, {signal}) failed: {error}"))
    }
}

pub fn terminate(pid: u32) {
    signal(pid, libc::SIGTERM).expect("SIGTERM test server");
}

/// Remove ECMA-48 control sequences, keeping the printable transcript: proof
/// a phrase was painted at some point, even with SGR between characters.
pub fn strip_terminal_controls(bytes: &[u8]) -> String {
    let mut printable = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            if bytes[index] >= 0x20 || matches!(bytes[index], b'\n' | b'\r' | b'\t') {
                printable.push(bytes[index]);
            }
            index += 1;
            continue;
        }

        index += 1;
        let Some(&kind) = bytes.get(index) else { break };
        index += 1;
        match kind {
            b'[' => {
                while let Some(&byte) = bytes.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            b']' | b'P' | b'^' | b'_' => {
                while let Some(&byte) = bytes.get(index) {
                    index += 1;
                    if byte == 0x07 {
                        break;
                    }
                    if byte == 0x1b && bytes.get(index) == Some(&b'\\') {
                        index += 1;
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    String::from_utf8_lossy(&printable).into_owned()
}

/// A live `phux watch --json` child whose NDJSON stdout is captured
/// off-thread: a watch must already be running when the edge happens, and an
/// edge superseded milliseconds later is visible only here.
pub struct WatchChild {
    child: Child,
    lines: Arc<Mutex<Vec<serde_json::Value>>>,
    label: String,
    poll: Duration,
    deadline: Duration,
}

impl WatchChild {
    /// Start `phux --socket <socket> watch TARGET --json <extra...>`.
    pub fn start(
        phux: &Path,
        socket: &Path,
        target: &str,
        extra: &[&str],
        poll: Duration,
        deadline: Duration,
    ) -> Self {
        let mut child = Command::new(phux)
            .arg("--socket")
            .arg(socket)
            .args(["watch", target, "--json"])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux watch");
        let stdout = child.stdout.take().expect("piped watch stdout");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    sink.lock().expect("watch line sink").push(value);
                }
            }
        });
        Self {
            child,
            lines,
            label: format!("phux watch {target} {extra:?}"),
            poll,
            deadline,
        }
    }

    /// Every line captured so far.
    pub fn seen(&self) -> Vec<serde_json::Value> {
        self.lines.lock().expect("watch line sink").clone()
    }

    /// Block until some captured line satisfies `want`, and return the whole
    /// transcript up to and including it. Panics with what it did see.
    pub fn await_line(
        &self,
        what: &str,
        want: impl Fn(&serde_json::Value) -> bool,
    ) -> Vec<serde_json::Value> {
        let end = Instant::now() + self.deadline;
        loop {
            let seen = self.seen();
            if seen.iter().any(&want) {
                return seen;
            }
            assert!(
                Instant::now() < end,
                "`{}` never printed {what} within {:?}; saw: {seen:?}",
                self.label,
                self.deadline
            );
            std::thread::sleep(self.poll);
        }
    }

    /// Block until an `agent_state` line reports `state`, and return the
    /// transcript. The identity of the surface matters: this is the ONLY way
    /// to observe a state the arbiter publishes and then supersedes on its
    /// next tick.
    pub fn await_agent_state(&self, state: &str) -> Vec<serde_json::Value> {
        self.await_line(&format!("agent_state {state}"), |line| {
            line["event"] == "agent_state" && line["state"] == state
        })
    }
}

impl Drop for WatchChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The index of the first captured line satisfying `want`, for asserting the
/// ORDER two observations arrived in rather than merely that both did.
pub fn first_index(
    lines: &[serde_json::Value],
    want: impl Fn(&serde_json::Value) -> bool,
) -> Option<usize> {
    lines.iter().position(want)
}

/// A real `phux attach` TUI on a pseudo-terminal: `/bin/sh` panes, a private
/// empty config dir (so the embedded defaults apply), every painted byte
/// drained into `transcript` so the PTY never backpressures the client.
/// Killed on drop.
pub struct PtyAttach {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    transcript: Arc<Mutex<Vec<u8>>>,
    pub config: tempfile::TempDir,
}

impl PtyAttach {
    /// `phux attach --socket SOCKET <args...>` at `cols x rows`, with `env`
    /// set after the defaults (`XDG_CONFIG_HOME` is `self.config`).
    pub fn start(
        socket: &Path,
        args: &[&str],
        size: (u16, u16),
        env: &[(&str, &std::ffi::OsStr)],
    ) -> Self {
        Self::start_with(socket, args, size, |_, command| {
            for (key, value) in env {
                command.env(key, value);
            }
        })
    }

    /// As [`Self::start`], with `configure` given the config dir and the
    /// command to adjust before the spawn.
    pub fn start_with(
        socket: &Path,
        args: &[&str],
        (cols, rows): (u16, u16),
        configure: impl FnOnce(&Path, &mut portable_pty::CommandBuilder),
    ) -> Self {
        use std::io::Read as _;

        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attach PTY");
        let config = tempfile::tempdir().expect("isolated config dir");
        let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_phux"));
        command.arg("attach");
        command.arg("--socket");
        command.arg(socket);
        command.args(args);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("XDG_CONFIG_HOME", config.path());
        configure(config.path(), &mut command);
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);
        let transcript = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&transcript);
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                sink.lock()
                    .expect("transcript lock")
                    .extend_from_slice(&bytes[..count]);
            }
        });
        let writer = pair.master.take_writer().expect("take PTY writer");
        Self {
            child,
            writer,
            transcript,
            config,
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to attach PTY");
        self.writer.flush().expect("flush attach PTY");
    }

    /// Everything painted so far, with terminal control sequences removed.
    pub fn painted(&self) -> String {
        strip_terminal_controls(&self.transcript.lock().expect("transcript lock"))
    }
}

impl Drop for PtyAttach {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::AutoSpawnedServer;

    /// The backstop must be the variable the server actually reads.
    #[test]
    fn the_idle_backstop_names_the_variable_the_server_reads() {
        let (key, value) = AutoSpawnedServer::IDLE_BACKSTOP;
        assert_eq!(key, phux::AUTO_SPAWN_IDLE_ENV);
        // The justfile's `AUTO_SPAWN_BACKSTOP` and docs/reference spell it
        // out, so a rename is a documentation change too, not a silent one.
        assert_eq!(key, "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE");
        assert!(
            value
                .parse::<u64>()
                .is_ok_and(|secs| (1..=86_400).contains(&secs)),
            "the backstop must be in the range the server accepts: {value}"
        );
    }

    /// An unused guard must not hang or spawn `phux` looking for a daemon
    /// that was never started. Cleanup is a no-op only when there is no pid
    /// *and* no socket file — not whenever the pid is unknown.
    #[test]
    fn unused_guard_drop_does_not_require_a_pid() {
        let server = AutoSpawnedServer::new(
            "/nonexistent/phux-e4qx",
            PathBuf::from("/nonexistent/phux-e4qx-absent.sock"),
        );
        drop(server);
    }
}
