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

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const SERVER_IDLE_LIMIT_SECS: &str = "600";
const SERVER_DEADLINE: Duration = Duration::from_secs(30);
const GRACEFUL_DEADLINE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// Owns a directly spawned server until it has actually been reaped.
pub struct ServerProcess {
    child: Child,
    socket: PathBuf,
}

impl ServerProcess {
    pub fn from_child(child: Child, socket: PathBuf) -> Self {
        Self { child, socket }
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

/// Tracks a daemonized auto-spawn by the PID reported over its live socket.
pub struct AutoSpawnedServer {
    phux: PathBuf,
    socket: PathBuf,
    pid: Option<u32>,
}

impl AutoSpawnedServer {
    /// The environment pair that bounds a daemon this type could not reap.
    ///
    /// It lives here, on the type whose existence *is* the hazard —
    /// constructing one means an auto-spawned daemon can outlive the harness
    /// (a runner killed outright, a panic before the PID was captured) —
    /// rather than
    /// in whichever harness happens to build a `Command`. The justfile's e2e
    /// recipe exports the same backstop, but every hermetic harness calls
    /// `env_clear()`, which wipes it before it can reach the daemon; that is
    /// exactly how this lane leaked immortal servers (phux-8y3o). Any harness
    /// that clears the environment re-arms it from here.
    ///
    /// The key is the server's own constant, not a second spelling of it, so
    /// a rename cannot leave the backstop silently disarmed.
    pub const IDLE_BACKSTOP: (&'static str, &'static str) =
        (phux::AUTO_SPAWN_IDLE_ENV, SERVER_IDLE_LIMIT_SECS);

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
            let output = Command::new(&self.phux)
                .args(["status", "--json", "--socket"])
                .arg(&self.socket)
                .stdin(Stdio::null())
                .output();
            if let Ok(output) = output
                && output.status.success()
                && let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                && let Some(pid) = doc["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok())
            {
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
        let Some(pid) = self.pid else {
            return Ok(());
        };

        if process_exists(pid) {
            let _ = Command::new(&self.phux)
                .args(["kill", "--server", "--socket"])
                .arg(&self.socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

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

/// Remove ECMA-48 control sequences while retaining printable transcript
/// bytes. This is intentionally a transcript view, not a screen emulator:
/// predicates must prove that a phrase was visibly painted at some point,
/// including renderers that put an SGR sequence between every character.
///
/// phux-k0cw.10: promoted here from `first_five_minutes_e2e` so a second PTY
/// test can read painted text without a second copy of the parser (phux-wcdq).
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

#[cfg(test)]
mod tests {
    use super::AutoSpawnedServer;

    /// The backstop has to be the variable the server actually reads. Pinning
    /// the spelling here closes the string-drift hole the two hand-written
    /// copies used to leave open: a harness arming `PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE`
    /// by literal would keep passing after the server renamed it, and the
    /// only symptom would be a daemon nobody reaped.
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
}
