//! The Unix socket's filesystem guard: a private parent directory, a private
//! socket file, and a stale-socket probe before bind.

use std::io;
use std::path::Path;
use tokio::net::UnixStream;
use tracing::{debug, warn};

use std::os::unix::fs::{
    DirBuilderExt, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};

use crate::runtime::{STALE_PROBE_TIMEOUT, ServerError};

/// Prepare and validate the parent directory of `socket_path`, so no other
/// user can plant or swap the socket.
///
/// A directory we create is made `0o700` and verified. An existing one is
/// validated, never mutated: it must be ours and not writable by others, or
/// sticky (like `/tmp`). Symlinks are resolved (macOS `/tmp` is one) and
/// the checks run against the canonical target.
pub(crate) fn prepare_socket_dir(socket_path: &Path) -> Result<(), ServerError> {
    let Some(parent) = socket_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let fail = |source| ServerError::PrepareDir {
        path: parent.to_path_buf(),
        source,
    };
    let expected_uid = rustix::process::geteuid().as_raw();
    let pre_existing = parent.exists();

    if !pre_existing {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(fail)?;
    }

    let real = std::fs::canonicalize(parent).map_err(fail)?;
    let metadata = std::fs::metadata(&real).map_err(fail)?;
    if !metadata.is_dir() {
        return Err(fail(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket parent is not a directory",
        )));
    }

    let mode = metadata.mode() & 0o7777;
    let sticky = mode & 0o1000 != 0;
    let others_may_write = mode & 0o022 != 0;

    if pre_existing {
        // Validate only. Ours-and-private, or sticky (the /tmp arrangement),
        // are both safe; anything else lets another user swap the socket.
        let ours_and_private = metadata.uid() == expected_uid && !others_may_write;
        if !ours_and_private && !sticky {
            return Err(fail(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket parent is writable by other users and not sticky; \
                 point --socket at a directory you own, or at a sticky temp dir",
            )));
        }
        return Ok(());
    }

    // We just created it, so it must be exactly what we asked for. A mismatch
    // means someone raced us between create and stat.
    if metadata.uid() != expected_uid || mode & 0o777 != 0o700 {
        return Err(fail(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket parent ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

/// Restrict a freshly bound UDS to its owning user.
pub(crate) fn secure_socket_file(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = std::fs::symlink_metadata(socket_path)?;
    let expected_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_socket() || metadata.uid() != expected_uid {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket is not an owner-controlled Unix socket",
        )));
    }
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    let secured = std::fs::symlink_metadata(socket_path)?;
    if secured.uid() != expected_uid || secured.mode() & 0o777 != 0o600 {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

/// Connect attempts before an existing socket entry counts as stale. A live
/// server whose listen backlog is momentarily full refuses a connect just
/// like a dead one; one refusal unlinked a busy production server's socket
/// and stranded every pane behind a second server.
const STALE_PROBE_ATTEMPTS: u32 = 4;

/// Pause between stale-probe attempts.
const STALE_PROBE_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

/// Handle the case where `socket_path` already exists. If something accepts a
/// connection on it within the probe budget, treat it as live and refuse to
/// start. Otherwise unlink the stale entry so `bind` can succeed. Callers hold
/// the [`SocketLock`] first, so no lock-holding server can be behind it.
pub(crate) async fn handle_existing_socket(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ServerError::Io(err)),
    };
    // Anything sitting in the way — socket, file, symlink — gets probed and
    // either rejected or removed.
    for attempt in 0..STALE_PROBE_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(STALE_PROBE_RETRY).await;
        }
        let connect =
            tokio::time::timeout(STALE_PROBE_TIMEOUT, UnixStream::connect(socket_path)).await;
        if let Ok(Ok(_stream)) = connect {
            return Err(ServerError::SocketBusy(socket_path.to_path_buf()));
        }
    }
    debug!(
        path = %socket_path.display(),
        file_type = ?metadata.file_type(),
        "removing stale socket entry",
    );
    std::fs::remove_file(socket_path).map_err(ServerError::Io)?;
    Ok(())
}

/// How long [`SocketLock::acquire`] waits for another server to let go:
/// long enough for a restart's old image to finish exiting.
const SOCKET_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The exclusive `flock` on [`phux_config::socket::server_lock_path`], held
/// for the server's lifetime: one server per socket path, and what client
/// probes read as "live" when the socket refuses a connect. The descriptor is
/// close-on-exec, so a graceful upgrade's new image takes the lock again.
#[derive(Debug)]
pub(crate) struct SocketLock {
    _file: std::fs::File,
}

impl SocketLock {
    /// Lock `socket_path`'s lock file, waiting up to [`SOCKET_LOCK_WAIT`].
    /// `WouldBlock` means another server holds it.
    pub(crate) fn acquire(socket_path: &Path) -> io::Result<Self> {
        use rustix::fs::{FlockOperation, flock};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(phux_config::socket::server_lock_path(socket_path))?;
        let deadline = std::time::Instant::now() + SOCKET_LOCK_WAIT;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("{} is held by another phux server", socket_path.display()),
                    ));
                }
                Err(errno) => return Err(errno.into()),
            }
        }
    }
}

/// The `(device, inode)` of the entry at `path`, if it can be stat'd.
pub(in crate::runtime) fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Unlink `path` only when it still resolves to the socket this server bound.
/// Unprovable ownership leaves the entry: a stale file is reaped by the next
/// stale probe, but deleting a live server's socket strands it.
pub(in crate::runtime) fn unlink_socket_if_ours(path: &Path, bound: Option<(u64, u64)>) {
    let Some(bound) = bound else {
        return;
    };
    match socket_identity(path) {
        Some(current) if current == bound => {
            if let Err(err) = std::fs::remove_file(path)
                && err.kind() != io::ErrorKind::NotFound
            {
                warn!(path = %path.display(), error = %err, "failed to unlink socket");
            }
        }
        Some(_) => {
            warn!(
                path = %path.display(),
                "socket path now belongs to another server; leaving it in place",
            );
        }
        None => {}
    }
}

#[cfg(test)]
mod socket_security_tests {
    use super::*;

    /// A directory phux creates is private; an existing one is validated and
    /// never chmod-ed: ours-and-private or sticky (like `/tmp`) is accepted,
    /// anything writable by others is refused. Symlinks are judged by their
    /// target (macOS `/tmp` is one).
    #[test]
    fn socket_parent_directory_rules() {
        use std::os::unix::fs::symlink;

        // (existing mode, reached through a symlink, accepted)
        let cases = [
            (None, false, true),
            (Some(0o777), false, false),
            (Some(0o1777), false, true),
            (Some(0o700), true, true),
            (Some(0o777), true, false),
        ];
        for (mode, via_symlink, accepted) in cases {
            let root = tempfile::tempdir().expect("tempdir");
            let target = root.path().join("runtime");
            if let Some(mode) = mode {
                std::fs::create_dir(&target).expect("dir");
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                    .expect("mode");
            }
            let parent = if via_symlink {
                let link = root.path().join("runtime-link");
                symlink(&target, &link).expect("symlink");
                link
            } else {
                target.clone()
            };
            let result = prepare_socket_dir(&parent.join("phux.sock"));
            let case = (mode, via_symlink);
            if accepted {
                result.unwrap_or_else(|err| panic!("{case:?} refused: {err}"));
            } else {
                assert!(
                    matches!(&result, Err(ServerError::PrepareDir { source, .. })
                        if source.kind() == io::ErrorKind::PermissionDenied),
                    "{case:?} accepted",
                );
            }
            let actual = std::fs::metadata(&target).expect("metadata").mode() & 0o7777;
            assert_eq!(actual, mode.unwrap_or(0o700), "{case:?} mode changed");
        }
    }
}
