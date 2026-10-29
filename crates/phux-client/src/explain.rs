//! Sentences for server replies the client did not expect.
//!
//! The server's own `Error` message verbatim, or, for a reply kind this
//! client cannot interpret, a version-skew pointer to `phux doctor`; never a
//! `Debug` dump of a wire enum.

pub use phux_client_runtime::session::unexpected_reply;
use std::path::Path;

use phux_protocol::wire::frame::CommandResult;

/// Explain a [`CommandResult`] a caller did not expect, attributed to `verb`:
/// `"{verb} failed: {message}"` for an `Error`, else [`unexpected_reply`].
#[must_use]
pub fn explain_unexpected(verb: &str, result: &CommandResult) -> String {
    match result {
        CommandResult::Error { message, .. } => format!("{verb} failed: {message}"),
        _ => unexpected_reply(verb),
    }
}

/// Explain a request withheld because the server does not advertise the
/// `feature` bit it needs: the server is older than this client, so the
/// remedy is `phux upgrade`, which hands it this phux in place.
#[must_use]
pub fn server_too_old(what: &str, feature: &str) -> String {
    format!(
        "the server is too old for {what} (it does not advertise {feature}), so nothing was \
         sent; run `phux upgrade` to hand it this phux, then retry"
    )
}

/// Lines for a live server that refused this binary's protocol.
///
/// `pid` must identify the owner of this socket, not merely an owned live
/// process. Even a known pid must be inspected again before signaling it.
/// Stopping this process closes only the panes it is running.
#[must_use]
pub fn old_server_recovery_lines(socket: &Path, pid: Option<i32>) -> Vec<String> {
    let mut lines = vec![format!(
        "the server on {} is a live phux server from an older release and does not speak \
         this binary's protocol",
        socket.display()
    )];
    let Some(socket) = socket.to_str() else {
        lines.push(
            "the socket path is not UTF-8; inspect its owner using an OS-native tool, \
                    then restart against that exact path; no shell recovery command is emitted"
                .to_owned(),
        );
        lines.push("stopping it closes only the panes it is running".to_owned());
        return lines;
    };
    lines.push(format!(
        "identify and confirm this socket's current owner with `lsof -nP -- {}`",
        shell_quote(socket)
    ));
    lines.push(stop_server_line(pid.filter(|pid| *pid > 0)));
    lines.push(format!(
        "then start a fresh server on the same socket under the selected profile with `{}`",
        restart_command(socket, &phux_config::instance::profile())
    ));
    lines.push("stopping it closes only the panes it is running".to_owned());
    lines.push(format!(
        "selected-profile server log: {}",
        server_log_path().display()
    ));
    lines
}

fn stop_server_line(pid: Option<i32>) -> String {
    pid.map_or_else(
        || {
            "inspect the reported positive owner PID with `ps -p PID -o pid=,ppid=,user=,args=`; \
            only after verifying it still owns this socket, stop that exact PID"
                .to_owned()
        },
        |pid| {
            format!(
                "inspect the socket owner with `ps -p {pid} -o pid=,ppid=,user=,args=`; \
             only if it still owns this socket, stop it with `kill {pid}`"
            )
        },
    )
}

/// Single-quote a shell word, including embedded apostrophes.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn restart_command(socket: &str, profile: &str) -> String {
    format!(
        "PHUX_PROFILE={} phux server --ensure --socket {}",
        shell_quote(profile),
        shell_quote(socket)
    )
}

/// The owner pid proven by this socket's peer credentials, when available.
///
/// Peer credentials are an OS fact available on connect, before `HELLO`.
/// `Connection::connect` drops them when `HELLO` is refused, so this dials
/// the socket without a handshake. A live same-uid health-log pid does not
/// prove socket ownership: it may name another profile or a reused pid.
#[must_use]
pub fn server_pid(socket: &Path) -> Option<i32> {
    peer_pid(socket)
}

/// Pid from the socket's peer credentials, or `None` when nothing is listening
/// or the platform omits the pid.
///
/// The dial is a standard-library connect, so it does not wait on `HELLO`.
/// Credentials are read through tokio, which is the stable `SO_PEERCRED` /
/// `LOCAL_PEEREPID` path (`std`'s `peer_cred` is still unstable). That read
/// needs a reactor: use the one already running, or a private current-thread
/// runtime when the caller is synchronous.
#[must_use]
pub fn peer_pid(socket: &Path) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(socket).ok()?;
        stream.set_nonblocking(true).ok()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio_peer_pid(stream)
        } else {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .ok()?;
            runtime.block_on(async move { tokio_peer_pid(stream) })
        }
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        None
    }
}

/// `SO_PEERCRED` / `LOCAL_PEEREPID` on a connected stream. The caller has a
/// tokio reactor entered. A peer uid other than `getuid` is not ours, so
/// the pid is not safe to print in `kill`.
#[cfg(unix)]
fn tokio_peer_pid(stream: std::os::unix::net::UnixStream) -> Option<i32> {
    let cred = tokio::net::UnixStream::from_std(stream)
        .ok()?
        .peer_cred()
        .ok()?;
    let pid = cred.pid()?;
    (pid > 0 && cred.uid() == our_uid()).then_some(pid)
}

/// Whether `kill(pid, 0)` succeeded.
///
/// `EPERM` (`Errno::PERM`) means the process exists and this caller may not
/// signal it. That is not alive for a kill hint.
#[must_use]
pub fn pid_is_alive(pid: i32) -> bool {
    signal_probe(pid).is_ok()
}

/// A live process owned by this user, not proof that it owns any socket.
///
/// Requires [`pid_is_alive`] and a real uid equal to `getuid`. Where the uid
/// cannot be read, returns `None`. Do not use this alone for a recovery kill
/// hint; use [`server_pid`] to establish ownership of the affected socket.
#[must_use]
pub fn owned_live_pid(pid: i32) -> Option<i32> {
    let uid = process_uid(pid)?;
    killable_owned_pid(pid, signal_probe(pid), uid, our_uid())
}

/// `Some(pid)` only when signal 0 succeeded and the process uid is ours.
///
/// `EPERM` is any `Err`, including a process that exists and is not ours.
/// A foreign uid is refused even when the signal check succeeds, which is
/// what happens when this process is privileged.
const fn killable_owned_pid(
    pid: i32,
    kill: Result<(), rustix::io::Errno>,
    process_uid: u32,
    our_uid: u32,
) -> Option<i32> {
    if pid > 0 && kill.is_ok() && process_uid == our_uid {
        Some(pid)
    } else {
        None
    }
}

#[cfg(unix)]
fn our_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn our_uid() -> u32 {
    0
}

/// `kill(pid, 0)`. An unrepresentable pid is not alive.
fn signal_probe(pid: i32) -> Result<(), rustix::io::Errno> {
    if pid <= 0 {
        return Err(rustix::io::Errno::INVAL);
    }
    #[cfg(unix)]
    {
        let Some(pid) = rustix::process::Pid::from_raw(pid) else {
            return Err(rustix::io::Errno::INVAL);
        };
        rustix::process::test_kill_process(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(rustix::io::Errno::NOSYS)
    }
}

/// Real uid of `pid` (`getuid` of that process).
///
/// Linux publishes it as the first field of `Uid:` in `/proc/<pid>/status`.
/// Where that file is absent the caller must not invent a kill target.
fn process_uid(pid: i32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    real_uid_from_status(&status)
}

/// First field of a `/proc/<pid>/status` `Uid:` line: real, effective,
/// saved, filesystem.
fn real_uid_from_status(status: &str) -> Option<u32> {
    let line = status.lines().find_map(|line| line.strip_prefix("Uid:"))?;
    line.split_whitespace().next()?.parse().ok()
}

/// `<state>/server.log`, the same path `phux_server::telemetry::server_log_path`
/// reports.
fn server_log_path() -> std::path::PathBuf {
    phux_config::instance::state_dir().join("server.log")
}

#[cfg(test)]
mod tests {
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::wire::frame::{CommandValue, ErrorCode};

    use super::*;

    #[test]
    fn error_message_passes_through_verbatim() {
        let result = CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "pane @9 does not exist".to_owned(),
        };
        assert_eq!(
            explain_unexpected("kill", &result),
            "kill failed: pane @9 does not exist",
        );
    }

    #[test]
    fn server_too_old_names_the_feature_and_the_upgrade() {
        let sentence = server_too_old("kill --server", "SHUTDOWN");
        assert!(sentence.contains("too old for kill --server"), "{sentence}");
        assert!(sentence.contains("SHUTDOWN"), "{sentence}");
        assert!(sentence.contains("`phux upgrade`"), "{sentence}");
    }

    /// An unknown reply kind names `phux doctor` and the protocol triple,
    /// and never leaks a `Debug` render.
    #[test]
    fn unknown_kind_names_doctor_and_the_protocol_triple() {
        let triple = format!(
            "client protocol {}.{}.{}",
            PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor, PROTOCOL_VERSION.patch,
        );
        for sentence in [
            explain_unexpected(
                "GET_STATE",
                &CommandResult::OkWith(CommandValue::Bytes(vec![1, 2, 3])),
            ),
            explain_unexpected("GET_STATE", &CommandResult::Ok),
        ] {
            assert!(
                sentence.contains("unexpected GET_STATE reply"),
                "{sentence}"
            );
            assert!(sentence.contains("run `phux doctor`"), "{sentence}");
            assert!(sentence.contains(&triple), "{sentence}");
            assert!(!sentence.contains('{'), "{sentence}");
        }
    }

    #[test]
    fn old_server_recovery_inspects_only_the_socket_owner_and_restarts_that_socket() {
        let socket = Path::new("/tmp/phux.sock");
        let with_pid = old_server_recovery_lines(socket, Some(4242)).join("\n");
        assert!(with_pid.contains("`kill 4242`"), "{with_pid}");
        assert!(
            with_pid.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
            "{with_pid}"
        );
        assert!(
            with_pid.contains("`lsof -nP -- '/tmp/phux.sock'`"),
            "{with_pid}"
        );
        assert!(
            with_pid.contains("phux server --ensure --socket '/tmp/phux.sock'`"),
            "{with_pid}"
        );
        assert!(with_pid.contains("closes only the panes"), "{with_pid}");
        assert!(with_pid.find("`ps -p").unwrap() < with_pid.find("`kill 4242`").unwrap());
        assert!(with_pid.contains("/server.log"), "{with_pid}");
        assert_recovery_is_clean(&with_pid);

        let unknown = old_server_recovery_lines(socket, None).join("\n");
        assert!(
            unknown.contains("`lsof -nP -- '/tmp/phux.sock'`"),
            "{unknown}"
        );
        assert!(
            unknown.contains("`ps -p PID -o pid=,ppid=,user=,args=`"),
            "{unknown}"
        );
        assert!(
            unknown.contains("phux server --ensure --socket '/tmp/phux.sock'`"),
            "{unknown}"
        );
        assert!(!unknown.contains("`kill "), "{unknown}");
        assert_recovery_is_clean(&unknown);
    }

    #[test]
    fn recovery_never_names_nonpositive_pids() {
        for pid in [0, -1, -4242] {
            let recovery =
                old_server_recovery_lines(Path::new("/tmp/old.sock"), Some(pid)).join("\n");
            assert!(!recovery.contains("`kill "), "{recovery}");
            assert!(recovery.contains("`ps -p PID "), "{recovery}");
            assert_eq!(killable_owned_pid(pid, Ok(()), 1000, 1000), None);
            assert!(!pid_is_alive(pid));
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_quotes_socket_paths_and_preserves_selected_profiles() {
        let directory = tempfile::tempdir().expect("private shell directory");
        let socket = "/tmp/a profile's $(touch NEVER_RUN); `echo bad`.sock";
        let quoted = shell_quote(socket);
        let lines = old_server_recovery_lines(Path::new(socket), None).join("\n");
        assert!(
            lines.contains(&format!("`lsof -nP -- {quoted}`")),
            "{lines}"
        );
        assert_recovery_is_clean(&lines);
        for profile in ["dev", "default", "another-profile"] {
            let command = restart_command(socket, profile);
            // Replace the phux executable with a harmless shell function to inspect
            // argv/environment. No phux process, socket, or signal is involved.
            let output = std::process::Command::new("sh")
                .current_dir(directory.path())
                .arg("-c")
                .arg(format!(
                    "phux() {{ printf '%s\\n' \"$PHUX_PROFILE\" \"$@\"; }}; {command}"
                ))
                .output()
                .expect("shell");
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).expect("UTF-8"),
                format!("{profile}\nserver\n--ensure\n--socket\n{socket}\n")
            );
        }
        let other = restart_command("/tmp/other.sock", "other");
        assert!(!other.contains(&quoted), "{other}");
        assert!(other.contains("PHUX_PROFILE='other'"), "{other}");
        assert!(!directory.path().join("NEVER_RUN").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_socket_gets_no_lossy_shell_command() {
        use std::os::unix::ffi::OsStrExt;
        let socket = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff.sock"));
        let lines = old_server_recovery_lines(socket, Some(4242)).join("\n");
        assert!(lines.contains("not UTF-8"), "{lines}");
        assert!(!lines.contains("`kill "), "{lines}");
        assert!(!lines.contains("phux server --ensure"), "{lines}");
    }

    /// `EPERM` is not a live pid, and neither is a process with another uid,
    /// even when signal 0 succeeds (this process may be privileged).
    #[test]
    fn a_kill_hint_names_only_our_live_pid() {
        let pid = 4242;
        let ours = 1000;
        assert_eq!(killable_owned_pid(pid, Ok(()), ours, ours), Some(pid));
        assert_eq!(
            killable_owned_pid(pid, Err(rustix::io::Errno::PERM), ours, ours),
            None,
            "EPERM means the process exists and is not ours"
        );
        assert_eq!(
            killable_owned_pid(pid, Ok(()), ours + 1, ours),
            None,
            "a foreign uid is not a kill target even when signal 0 succeeds"
        );
        assert_eq!(
            killable_owned_pid(pid, Err(rustix::io::Errno::SRCH), ours, ours),
            None
        );
    }

    #[test]
    fn status_real_uid_is_the_first_uid_field() {
        let status = "Name:\tphux\nUid:\t1000\t0\t0\t1000\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(real_uid_from_status(status), Some(1000));
        assert_eq!(real_uid_from_status("Name:\tphux\n"), None);
    }

    /// An owned live pid alone does not establish socket ownership.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_owned_live_pid_does_not_own_a_missing_socket() {
        let pid = i32::try_from(std::process::id()).expect("pid");
        assert_eq!(process_uid(pid), Some(our_uid()));
        assert_eq!(owned_live_pid(pid), Some(pid));
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(server_pid(&dir.path().join("missing.sock")), None);
    }

    #[cfg(unix)]
    struct SocketOwner(std::process::Child);

    #[cfg(unix)]
    impl SocketOwner {
        fn start(socket: &Path, profile: &str) -> Self {
            let directory = socket.parent().expect("socket directory");
            let child =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "explain::tests::socket_owner_fixture",
                        "--ignored",
                    ])
                    .env("PHUX_RECOVERY_TEST_SOCKET", socket)
                    .env("PHUX_PROFILE", profile)
                    .env("HOME", directory)
                    .env("XDG_STATE_HOME", directory.join("state"))
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .expect("socket owner child");
            let owner = Self(child);
            for _ in 0..200 {
                if socket.exists() {
                    return owner;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("socket owner did not bind {}", socket.display());
        }
    }

    #[cfg(unix)]
    impl Drop for SocketOwner {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A private child process gives two sockets genuinely different peer pids.
    #[cfg(unix)]
    #[test]
    #[ignore = "invoked only by the socket-owner regression test"]
    fn socket_owner_fixture() {
        let Some(socket) = std::env::var_os("PHUX_RECOVERY_TEST_SOCKET") else {
            return;
        };
        let socket = Path::new(&socket);
        let state = phux_config::instance::state_dir();
        std::fs::create_dir_all(&state).expect("private state");
        std::fs::write(
            state.join("server-starts.log"),
            format!("100 {} 0.9.0\n", std::process::id()),
        )
        .expect("private health log");
        assert_eq!(
            server_pid(&socket.with_extension("missing")),
            None,
            "a live same-uid health-record pid is not proof of socket ownership"
        );
        let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind child socket");
        // Keep this probe connected: macOS can no longer report peer credentials
        // once the remote stream has closed. The parent reaps this fixture.
        let (_peer, _) = listener.accept().expect("accept owner probe");
        loop {
            std::thread::park();
        }
    }

    #[cfg(unix)]
    #[test]
    fn server_pid_names_only_the_owner_of_each_supplied_socket() {
        let directory = tempfile::tempdir().expect("private directory");
        let socket_a = directory.path().join("a.sock");
        let socket_b = directory.path().join("b.sock");
        let owner_a = SocketOwner::start(&socket_a, "recovery-a");
        let owner_b = SocketOwner::start(&socket_b, "recovery-b");
        let pid_a = i32::try_from(owner_a.0.id()).expect("positive pid");
        let pid_b = i32::try_from(owner_b.0.id()).expect("positive pid");
        assert_ne!(pid_a, pid_b);
        assert_eq!(server_pid(&socket_a), Some(pid_a));
        assert_eq!(server_pid(&socket_b), Some(pid_b));
        assert_eq!(server_pid(&directory.path().join("missing.sock")), None);
    }

    fn assert_recovery_is_clean(text: &str) {
        for forbidden in [
            "phux upgrade",
            "stale",
            "survive",
            "remove",
            "pkill",
            "killall",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }
}
