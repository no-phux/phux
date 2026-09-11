//! `server --resume <fd>` support (ADR-0032): read the handoff state blob from
//! an inherited descriptor and adopt the inherited `UnixListener`, so the
//! re-exec'd image continues serving on the already-bound socket without a
//! rebind race.

use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{FromRawFd, RawFd};

use tokio::net::UnixListener;

use super::ServerError;
use crate::transport::UdsListener;
use crate::upgrade::blob::StateBlob;

/// Read and deserialize the [`StateBlob`] from the inherited handoff
/// descriptor, taking ownership of `fd`.
///
/// Seeks to the start first: the orchestrator wrote the blob through the same
/// open file description, so the shared offset sits at EOF after the exec.
///
/// # Errors
/// [`ServerError::Io`] if the descriptor can't be read; [`ServerError::Resume`]
/// if the bytes aren't a valid blob this binary understands.
pub(super) fn read_blob_from_fd(fd: RawFd) -> Result<StateBlob, ServerError> {
    // SAFETY: `fd` is the inherited blob descriptor (a memfd / temp file the
    // orchestrator wrote and left open across the exec); we take sole
    // ownership, so it is closed when `file` drops.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    StateBlob::from_bytes(&bytes).map_err(ServerError::Resume)
}

/// Adopt the inherited `UnixListener` (its `FD_CLOEXEC` cleared before the
/// exec) so the socket stays bound with no rebind race.
///
/// # Errors
/// [`ServerError::Io`] if the descriptor can't be put in non-blocking mode or
/// registered with the tokio reactor.
pub(super) fn adopt_uds_listener(fd: RawFd) -> Result<UdsListener, ServerError> {
    // SAFETY: `fd` is the inherited listening socket, owned solely by us now.
    let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;
    Ok(UdsListener::new(listener))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Incoming;
    use crate::upgrade::blob::{BLOB_VERSION, Counters};
    use std::io::Write;
    use std::os::fd::IntoRawFd;

    fn empty_blob(listener_fd: RawFd) -> StateBlob {
        StateBlob {
            version: BLOB_VERSION,
            listener_fd,
            counters: Counters {
                next_session_wire_id: 1,
                next_terminal_wire_id: 1,
                next_window_wire_id: 1,
                next_touch_timestamp: 1,
                server_instance: None,
            },
            sessions: Vec::new(),
            windows: Vec::new(),
            panes: Vec::new(),
        }
    }

    #[test]
    fn reads_blob_from_a_seekable_fd_at_eof() {
        let blob = empty_blob(5);
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(&blob.to_bytes().expect("serialize"))
            .expect("write");
        // Leave the offset at EOF deliberately — `read_blob_from_fd` must seek.
        let fd = file.into_raw_fd();

        let read = read_blob_from_fd(fd).expect("read blob");
        assert_eq!(read, blob);
    }

    #[tokio::test]
    async fn adopts_an_inherited_listener_and_accepts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("resume.sock");
        let std_listener = std::os::unix::net::UnixListener::bind(&path).expect("bind listener");
        // Relinquish ownership; `adopt_uds_listener` re-takes it by raw fd,
        // exactly as the re-exec'd image inherits it.
        let fd = std_listener.into_raw_fd();

        let uds = adopt_uds_listener(fd).expect("adopt listener");

        let (client, accepted) =
            tokio::join!(tokio::net::UnixStream::connect(&path), uds.accept(),);
        client.expect("client connects to the adopted socket");
        accepted.expect("adopted listener accepts the connection");
    }
}
