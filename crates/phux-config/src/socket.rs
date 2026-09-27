//! Default Unix-domain-socket path resolution and liveness probing.
//!
//! Lives in `phux-config` (not `phux-server`) so thin consumers — the MCP
//! adapter, CLI verbs, future satellites — can agree with the daemon on one
//! socket location without pulling in the heavy server crate (phux-93b).

use std::ffi::OsString;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::instance;

/// Resolve the default Unix-domain-socket path.
///
/// Precedence (the daemon binds this; every consumer connects to it):
/// 1. `$PHUX_SOCKET` if set — an explicit `--socket` flag still overrides it
///    at the call sites that take one;
/// 2. the profile-scoped runtime directory ([`instance::runtime_dir`]), which
///    is `$XDG_RUNTIME_DIR/phux[-<profile>]` or `/tmp/phux-$USER[-<profile>]`.
///
/// The profile suffix is what keeps a development build off the production
/// socket; see [`instance`].
#[must_use]
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PHUX_SOCKET") {
        return PathBuf::from(path);
    }
    instance::runtime_dir().join("phux.sock")
}

/// The day-to-day installation's socket in this environment.
#[must_use]
pub fn production_socket_path() -> PathBuf {
    instance::default_profile_runtime_dir().join("phux.sock")
}

/// Refuse to let a development build touch the day-to-day server.
///
/// Every local connection and every server bind passes through this. A
/// [`instance::BuildKind::Dev`] process aimed at the production socket, by
/// `--socket`, `PHUX_SOCKET`, `PHUX_PROFILE=default`, or a symlink, gets an
/// error naming the dev server to use instead. There is deliberately no
/// override: a dev build that could reach production is how a day of panes
/// gets replaced by a debug image.
///
/// # Errors
///
/// The refusal, as a message for the caller to surface.
pub fn refuse_dev_on_production(socket: &Path) -> Result<(), String> {
    if instance::build_kind() != instance::BuildKind::Dev {
        return Ok(());
    }
    refuse_if_same_socket(socket, &production_socket_path())
}

fn refuse_if_same_socket(socket: &Path, production: &Path) -> Result<(), String> {
    if !same_socket(socket, production) {
        return Ok(());
    }
    Err(format!(
        "refusing to use the production phux socket {} from a development build; \
         dev builds run their own server under the `{}` profile. Use `phux` from \
         the installed release for the production server, and never copy a dev \
         build over the installed binary",
        production.display(),
        instance::DEV_PROFILE,
    ))
}

/// Whether two socket paths name the same file, resolving symlinked
/// directories (`/tmp` is `/private/tmp` on macOS). The socket itself may
/// not exist yet, so only its directory is canonicalised.
fn same_socket(a: &Path, b: &Path) -> bool {
    fn resolved(path: &Path) -> PathBuf {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return path.to_path_buf();
        };
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        std::fs::canonicalize(dir).map_or_else(|_| path.to_path_buf(), |dir| dir.join(name))
    }
    a == b || resolved(a) == resolved(b)
}

/// The advisory lock serialising server auto-spawn within one profile.
///
/// A sibling of the socket rather than the socket itself: the socket is
/// unlinked and recreated across server generations, and a lock whose inode
/// changes underneath its holders is not a lock.
#[must_use]
pub fn spawn_lock_path(socket: &Path) -> PathBuf {
    let lock_name = match socket.file_name() {
        Some(name) if name == "phux.sock" => OsString::from("spawn.lock"),
        Some(name) => {
            let mut lock_name = name.to_os_string();
            lock_name.push(".spawn.lock");
            lock_name
        }
        None => OsString::from("spawn.lock"),
    };
    socket
        .parent()
        .map_or_else(|| PathBuf::from("/tmp"), Path::to_path_buf)
        .join(lock_name)
}

/// What a probe of a socket path found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    /// Nothing at the path. A server may be spawned.
    Absent,
    /// A socket file exists but nothing accepts on it: the server it belonged
    /// to died without unlinking (SIGKILL, panic, power loss, a supervisor
    /// tearing it down). The entry must be removed before a new server can
    /// bind, and — critically — a client must treat this as "no server" and
    /// auto-spawn rather than reporting a dead socket to the user.
    Stale,
    /// Something accepted a connection. A server is live; do not touch it.
    Live,
}

impl SocketState {
    /// Whether a server should be started for this state.
    #[must_use]
    pub const fn needs_server(self) -> bool {
        matches!(self, Self::Absent | Self::Stale)
    }
}

/// Classify `path` by attempting a connection.
///
/// Existence is **not** liveness. A socket file outlives the process that
/// bound it, so gating auto-spawn on `Path::exists` wedges every subsequent
/// invocation the moment a server dies uncleanly: the file is there, so no
/// server is started, and nothing is listening, so the connection fails. This
/// probe is the difference between "phux recovers on its own" and "phux is
/// broken until someone knows to `rm` a socket" (phux-zomb.1).
///
/// A connect to an unbound Unix socket fails immediately with
/// `ECONNREFUSED`; no timeout is needed and none is imposed. Errors other
/// than "refused" or "not found" (notably `EPERM` on a socket owned by
/// another user) classify as [`SocketState::Live`] — the conservative
/// direction, since the cost of a false `Stale` is unlinking a healthy
/// server's socket, while the cost of a false `Live` is a clear error
/// message.
#[must_use]
pub fn probe(path: &Path) -> SocketState {
    if !path.exists() {
        return SocketState::Absent;
    }
    match UnixStream::connect(path) {
        Ok(stream) => {
            // Drop immediately: this is a liveness probe, not a session. The
            // server sees a connect-then-disconnect with no HELLO and reaps
            // the peer without allocating one.
            drop(stream);
            SocketState::Live
        }
        Err(err) if is_unbound(&err) => SocketState::Stale,
        Err(_) => SocketState::Live,
    }
}

/// Whether a connect error means "no process is bound here".
fn is_unbound(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

/// Remove a socket entry previously classified [`SocketState::Stale`].
///
/// Re-probes first. Between the original probe and this call a server may
/// have bound the path — the auto-spawn lock makes that unlikely but not
/// impossible, and unlinking a live server's socket is the exact failure this
/// module exists to prevent. A path that has become live is left alone and
/// reported as `Ok(false)`.
///
/// # Errors
/// Propagates the unlink failure, except `NotFound` — another party winning
/// the same cleanup is success, not an error.
pub fn reap_stale(path: &Path) -> io::Result<bool> {
    if probe(path) != SocketState::Stale {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn production_socket_is_refused_through_a_symlinked_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let production = real.join("phux.sock");

        let refusal = refuse_if_same_socket(&link.join("phux.sock"), &production)
            .expect_err("the same socket through a symlink is still production");
        assert!(refusal.contains("development build"), "{refusal}");
        assert!(refuse_if_same_socket(&real.join("other.sock"), &production).is_ok());
        assert!(refuse_if_same_socket(&dir.path().join("phux.sock"), &production).is_ok());
    }

    #[test]
    fn test_binaries_may_not_use_the_production_socket() {
        // Tests are dev builds, so the live guard applies to them too.
        assert!(refuse_dev_on_production(&production_socket_path()).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(refuse_dev_on_production(&dir.path().join("phux.sock")).is_ok());
    }

    #[test]
    fn absent_path_probes_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        assert_eq!(probe(&path), SocketState::Absent);
        assert!(probe(&path).needs_server());
    }

    #[test]
    fn a_bound_socket_probes_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        let _listener = UnixListener::bind(&path).expect("bind");
        assert_eq!(probe(&path), SocketState::Live);
        assert!(!probe(&path).needs_server());
    }

    #[test]
    fn a_socket_whose_server_died_probes_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        // Closing the listener without unlinking is exactly what a SIGKILLed
        // server leaves behind — the file remains, nothing accepts.
        drop(listener);
        assert!(path.exists(), "the socket file must outlive the listener");
        assert_eq!(probe(&path), SocketState::Stale);
        assert!(
            probe(&path).needs_server(),
            "a stale socket must not block auto-spawn — this is phux-zomb.1"
        );
    }

    #[test]
    fn reaping_removes_a_stale_entry_and_unblocks_bind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        drop(UnixListener::bind(&path).expect("bind"));
        assert!(reap_stale(&path).expect("reap"), "stale entry is removed");
        assert_eq!(probe(&path), SocketState::Absent);
        // The whole point: a fresh server can now bind.
        UnixListener::bind(&path).expect("rebind after reap");
    }

    #[test]
    fn reaping_refuses_to_unlink_a_live_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        let _listener = UnixListener::bind(&path).expect("bind");
        assert!(
            !reap_stale(&path).expect("reap"),
            "live socket is untouched"
        );
        assert!(path.exists(), "a live server keeps its socket");
    }

    #[test]
    fn reaping_a_missing_path_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!reap_stale(&dir.path().join("nope.sock")).expect("reap"));
    }

    #[test]
    fn the_spawn_lock_is_a_sibling_of_the_socket() {
        let socket = default_socket_path();
        let lock = spawn_lock_path(&socket);
        assert_eq!(lock.parent(), socket.parent());
        assert_ne!(lock, socket, "the lock must not be the socket itself");
        assert_eq!(lock.file_name().unwrap(), "spawn.lock");
    }

    #[test]
    fn custom_sockets_have_distinct_spawn_locks() {
        let first = Path::new("/tmp/phux/first.sock");
        let second = Path::new("/tmp/phux/second.sock");

        assert_ne!(spawn_lock_path(first), spawn_lock_path(second));
        assert_eq!(
            spawn_lock_path(first),
            Path::new("/tmp/phux/first.sock.spawn.lock")
        );
    }
}
