//! Directly spawn a trusted handler in its own process group.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags, posix_spawn};
use nix::sys::signal::{SigSet, Signal as NixSignal};
use rustix::process::{Pid, Signal, WaitOptions, kill_process, kill_process_group, waitpid};
use tokio::io::AsyncWriteExt as _;
use tokio::net::unix::pipe;
use tokio::signal::unix::{SignalKind, signal};

pub(super) async fn invoke(
    program: &OsStr,
    args: &[OsString],
    line: &str,
) -> io::Result<ExitStatus> {
    let mut exited = signal(SignalKind::child())?;
    let (mut parent, stdin) = pipe::pipe()?;
    let stdin = stdin.into_blocking_fd()?;
    let mut child = spawn(program, args, &stdin)?;
    drop(stdin);

    // A handler may ignore its input. A closed stdin alone is not a failure.
    if let Err(err) = parent.write_all(line.as_bytes()).await
        && err.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(err);
    }
    drop(parent);

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        exited.recv().await;
    }
}

fn spawn(program: &OsStr, args: &[OsString], stdin: &impl AsRawFd) -> io::Result<Handler> {
    let path = executable(program)?;
    let command_line = std::iter::once(program)
        .chain(args.iter().map(OsString::as_os_str))
        .map(|arg| CString::new(arg.as_encoded_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut envp = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        let mut entry = OsString::from("PATH=");
        entry.push(path);
        envp.push(CString::new(entry.into_encoded_bytes())?);
    }

    let mut files = PosixSpawnFileActions::init()?;
    files.add_dup2(stdin.as_raw_fd(), 0)?;
    let mut attrs = PosixSpawnAttr::init()?;
    attrs.set_pgroup(nix::unistd::Pid::from_raw(0))?;
    let mut defaults = SigSet::empty();
    defaults.add(NixSignal::SIGPIPE);
    attrs.set_sigdefault(&defaults)?;
    attrs.set_flags(
        PosixSpawnFlags::POSIX_SPAWN_SETPGROUP | PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF,
    )?;

    // Unlike execvp or posix_spawnp, posix_spawn does not interpret ENOEXEC
    // as an instruction to pass an executable text file to a shell.
    let pid = posix_spawn(&path, &files, &attrs, &command_line, &envp)?;
    let pid = Pid::from_raw(pid.as_raw()).ok_or_else(|| io::Error::other("invalid handler PID"))?;
    Ok(Handler(Some(pid)))
}

fn executable(program: &OsStr) -> io::Result<PathBuf> {
    if program.as_encoded_bytes().contains(&b'/') {
        return Ok(PathBuf::from(program));
    }
    program
        .to_str()
        .and_then(phux_plugin::executable_path)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "executable {} was not found on PATH",
                    program.to_string_lossy()
                ),
            )
        })
}

struct Handler(Option<Pid>);

impl Handler {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let Some(pid) = self.0 else {
            return Ok(None);
        };
        let status = match rustix::io::retry_on_intr(|| waitpid(Some(pid), WaitOptions::NOHANG)) {
            Ok(Some((_, status))) if status.exited() || status.signaled() => status,
            Ok(_) => return Ok(None),
            Err(rustix::io::Errno::CHILD) => {
                // If it was already reaped, the PID is no longer ours to signal.
                self.0 = None;
                return Err(rustix::io::Errno::CHILD.into());
            }
            Err(err) => return Err(err.into()),
        };
        self.0 = None;
        Ok(Some(ExitStatus::from_raw(status.as_raw())))
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // Signal before reaping the leader: the PID cannot be recycled.
            let _ = kill_process_group(pid, Signal::KILL);
            let _ = kill_process(pid, Signal::KILL);
            reap_cancelled(pid);
        }
    }
}

fn reap_cancelled(pid: Pid) {
    // Drop also runs when the runtime is about to exit, so an async reaper
    // would itself be cancelled. Bound the synchronous fallback in case
    // SIGKILL is pending on uninterruptible kernel I/O.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match rustix::io::retry_on_intr(|| waitpid(Some(pid), WaitOptions::NOHANG)) {
            Ok(Some(_)) | Err(rustix::io::Errno::CHILD) => return,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                warn!("on: handler {pid} did not exit within one second of SIGKILL");
                return;
            }
            Err(err) => {
                warn!("on: could not reap handler {pid}: {err}");
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_a_handler_reaps_its_child() {
        let (_parent, stdin) = pipe::pipe().unwrap();
        let child = spawn(
            OsStr::new("/bin/sleep"),
            &["30".into()],
            &stdin.into_blocking_fd().unwrap(),
        )
        .unwrap();
        let pid = child.0.unwrap();
        drop(child);
        assert!(matches!(
            waitpid(Some(pid), WaitOptions::NOHANG),
            Err(rustix::io::Errno::CHILD)
        ));
    }
}
