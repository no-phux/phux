//! Private CLOEXEC exec-status channel. Never carried in an upgrade blob.
use phux_core::{LaunchDiagnostic, LaunchStage};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub(super) fn channel() -> io::Result<(OwnedFd, OwnedFd)> {
    // std creates both descriptors CLOEXEC on every supported Unix platform.
    let (reader, writer) = UnixStream::pair()?;
    reader.set_nonblocking(true)?;
    Ok((reader.into(), writer.into()))
}

pub(super) fn confirm(pid: libc::pid_t, reader: OwnedFd) -> Result<(), LaunchDiagnostic> {
    let result = read_status(UnixStream::from(reader), Duration::from_secs(2));
    if result.is_err() {
        // This exact child was spawned above and has not been handed to an
        // actor/reaper. Never spawn again: timeout may follow an executed effect.
        let child = nix::unistd::Pid::from_raw(pid);
        let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
        while nix::sys::wait::waitpid(child, None) == Err(nix::errno::Errno::EINTR) {}
    }
    result
}

const fn uncertain() -> LaunchDiagnostic {
    LaunchDiagnostic {
        stage: LaunchStage::Handshake,
        errno: None,
    }
}

fn read_status(mut reader: UnixStream, timeout: Duration) -> Result<(), LaunchDiagnostic> {
    let deadline = Instant::now() + timeout;
    let mut bytes = [0; 6];
    let mut used = 0;
    loop {
        match reader.read(&mut bytes[used..]) {
            Ok(0) => {
                return if used == 1 && bytes[0] == 0xa5 {
                    Ok(())
                } else {
                    Err(uncertain())
                };
            }
            Ok(count) => {
                used += count;
                if used == bytes.len() {
                    return Err(decode_failure(bytes));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_readable(&reader, deadline)?;
            }
            Err(_) => return Err(uncertain()),
        }
    }
}

const fn decode_failure(bytes: [u8; 6]) -> LaunchDiagnostic {
    if bytes[0] != 0xa5 {
        return uncertain();
    }
    let stage = match bytes[1] {
        1 => LaunchStage::Cwd,
        2 => LaunchStage::Exec,
        3 => LaunchStage::Spawn,
        _ => return uncertain(),
    };
    LaunchDiagnostic {
        stage,
        errno: Some(i32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]])),
    }
}

fn wait_readable(reader: &UnixStream, deadline: Instant) -> Result<(), LaunchDiagnostic> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(uncertain());
    }
    let timeout = i32::try_from(remaining.as_millis())
        .unwrap_or(i32::MAX)
        .max(1);
    let mut poll = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live initialized pollfd, no retained pointers.
    let rc = unsafe { libc::poll(&raw mut poll, 1, timeout) };
    if rc > 0 || (rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted) {
        Ok(())
    } else {
        Err(uncertain())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn early_eof_and_incomplete_status_are_not_exec_success() {
        for bytes in [vec![], vec![0xa5, 2], vec![0]] {
            let (reader, writer) = channel().unwrap();
            let mut writer = UnixStream::from(writer);
            writer.write_all(&bytes).unwrap();
            drop(writer);
            assert_eq!(
                read_status(UnixStream::from(reader), Duration::from_millis(10)),
                Err(uncertain())
            );
        }
    }

    #[test]
    fn held_status_descriptor_has_a_deadline() {
        let (reader, _writer) = channel().unwrap();
        assert_eq!(
            read_status(UnixStream::from(reader), Duration::from_millis(10)),
            Err(uncertain())
        );
    }
}
