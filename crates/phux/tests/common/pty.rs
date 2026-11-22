//! Clients on a pseudo-terminal: a real `phux attach` TUI ([`PtyAttach`])
//! and a one-shot PTY transcript ([`pty_transcript`]).

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "shared fail-fast integration-test harness"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{POLL, ambient, strip_terminal_controls};

/// `phux attach <args...>` with the harness isolation env applied last.
///
/// `configure` may `env_clear`. The release-check opt-out is sealed
/// afterwards: a private state dir still permits a live lookup, and a cached
/// newer release still paints a modal that consumes the next scripted key.
pub(super) fn isolated_attach_command(
    args: &[&str],
    config: &Path,
    configure: impl FnOnce(&Path, &mut portable_pty::CommandBuilder),
) -> portable_pty::CommandBuilder {
    isolated_client_command("attach", args, config, configure)
}

fn isolated_client_command(
    verb: &str,
    args: &[&str],
    config: &Path,
    configure: impl FnOnce(&Path, &mut portable_pty::CommandBuilder),
) -> portable_pty::CommandBuilder {
    let mut command = portable_pty::CommandBuilder::new(crate::runner::phux_bin());
    for key in ambient::ambient_phux_keys() {
        command.env_remove(key);
    }
    command.arg(verb);
    command.args(args);
    command.env("SHELL", "/bin/sh");
    command.env("TERM", "xterm-256color");
    command.env("RUST_LOG", "off");
    command.env("XDG_CONFIG_HOME", config);
    command.env("XDG_STATE_HOME", &ambient::isolated_xdg().state);
    configure(config, &mut command);
    command.env(ambient::NO_UPDATE_CHECK_ENV, ambient::NO_UPDATE_CHECK_VALUE);
    command
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
    master: Box<dyn portable_pty::MasterPty + Send>,
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
        size: (u16, u16),
        configure: impl FnOnce(&Path, &mut portable_pty::CommandBuilder),
    ) -> Self {
        let mut attach_args = vec!["--socket", socket.to_str().expect("UTF-8 socket path")];
        attach_args.extend_from_slice(args);
        Self::start_dialing(&attach_args, size, configure)
    }

    /// `phux attach <args...>` with no `--socket` injected, for an attach
    /// that dials by `--quic`, `--ws`, or `--remote`.
    pub fn start_dialing(
        args: &[&str],
        (cols, rows): (u16, u16),
        configure: impl FnOnce(&Path, &mut portable_pty::CommandBuilder),
    ) -> Self {
        Self::start_client("attach", args, (cols, rows), configure)
    }

    /// Run a different interactive client mode with the same PTY isolation.
    pub fn start_client(
        verb: &str,
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
        let command = isolated_client_command(verb, args, config.path(), configure);
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
            master: pair.master,
        }
    }

    pub fn terminal_is_cooked(&self) -> bool {
        use std::os::fd::BorrowedFd;
        let raw = self.master.as_raw_fd().expect("master descriptor");
        // SAFETY: the master remains owned by self for this borrow.
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        let mode = rustix::termios::tcgetattr(fd).expect("client terminal modes");
        mode.local_modes
            .contains(rustix::termios::LocalModes::ECHO | rustix::termios::LocalModes::ICANON)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resize client PTY");
    }

    pub fn signal(&self, signal: i32) {
        super::signal(self.child.process_id().expect("client pid"), signal).expect("signal client");
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to attach PTY");
        self.writer.flush().expect("flush attach PTY");
    }

    /// Whether the `phux attach` process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Poll until the client exits, or `None` once `within` passes (the
    /// client is then killed on drop).
    pub fn exit_within(&mut self, within: Duration) -> Option<portable_pty::ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().expect("client try_wait") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Everything painted so far, with terminal control sequences removed.
    pub fn painted(&self) -> String {
        strip_terminal_controls(&self.transcript.lock().expect("transcript lock"))
    }

    /// Wait for the client to exit and return its status; panic with the
    /// transcript once `within` passes.
    pub fn wait_exit(&mut self, within: Duration) -> portable_pty::ExitStatus {
        self.exit_within(within).unwrap_or_else(|| {
            panic!(
                "attach client did not exit within {within:?}; painted text was:\n{}",
                self.painted()
            )
        })
    }

    /// Wait until every phrase has been painted; panic with the transcript.
    pub fn await_painted(&self, phrases: &[&str], within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            let painted = self.painted();
            if phrases.iter().all(|phrase| painted.contains(phrase)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never painted {phrases:?}; painted text was:\n{painted}"
            );
            std::thread::sleep(POLL);
        }
    }
}

/// Clear `cmd`'s environment down to the inherited `PATH` and `TMPDIR`, so
/// no ambient `PHUX_*` from a live pane reaches the child.
pub fn clear_pty_env(cmd: &mut portable_pty::CommandBuilder) {
    cmd.env_clear();
    for key in ["PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
}

/// Run `phux ARGS` on a fresh 80x24 PTY (what an attach's TTY preflight
/// needs), with `configure` setting its environment, then `TERM` and the
/// update-check opt-out. Output is read on a thread, so a quiet child cannot
/// park the deadline. Reading stops `settle` after `done` first holds, when
/// the output closes, or at `deadline`; the child is then killed, since an
/// attach would otherwise hold the PTY forever. Returns everything read.
pub fn pty_transcript(
    args: &[&str],
    configure: impl FnOnce(&mut portable_pty::CommandBuilder),
    done: impl Fn(&str) -> bool,
    settle: Duration,
    deadline: Duration,
) -> String {
    use std::io::Read as _;

    let pty = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = portable_pty::CommandBuilder::new(crate::runner::phux_bin());
    cmd.args(args);
    configure(&mut cmd);
    cmd.env("TERM", "xterm-256color");
    cmd.env(ambient::NO_UPDATE_CHECK_ENV, ambient::NO_UPDATE_CHECK_VALUE);
    let mut child = pty.slave.spawn_command(cmd).expect("spawn phux");
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().expect("clone reader");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let start = Instant::now();
    let mut seen = String::new();
    let mut settle_until: Option<Instant> = None;
    loop {
        let budget = settle_until.map_or_else(
            || deadline.saturating_sub(start.elapsed()),
            |until| until.saturating_duration_since(Instant::now()),
        );
        if budget.is_zero() {
            break;
        }
        let Ok(chunk) = rx.recv_timeout(budget) else {
            break;
        };
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if settle_until.is_none() && done(&seen) {
            settle_until = Some(Instant::now() + settle);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    seen
}

impl Drop for PtyAttach {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
