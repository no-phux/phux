use super::*;
use crate::actor::WriteCompletion;
use crate::actor::WriteCompletionSink;
use nix::sys::termios::{InputFlags, LocalFlags, OutputFlags};
use portable_pty::CommandBuilder;
use std::time::Duration;

/// Short on purpose: an absence check only gets easier under load.
const NOTHING_ARRIVES_WINDOW: Duration = Duration::from_millis(300);
const DELIVERY_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn canonical_limit_counts_the_longest_line() {
    let mut many_short = Vec::new();
    for _ in 0..50 {
        many_short.extend(std::iter::repeat_n(b'x', 100));
        many_short.push(b'\n');
    }
    let mut overlong_then_cr = vec![b'a'; 1800];
    overlong_then_cr.push(b'\r');
    let mut cr_at_limit = vec![b'a'; 1024];
    cr_at_limit.push(b'\r');
    cr_at_limit.extend(std::iter::repeat_n(b'b', 10));
    let cases: &[(&[u8], bool, bool)] = &[
        (&[b'a'; 1024], true, false),
        (&[b'a'; 1025], true, true),
        (&many_short, true, false),
        // The terminating CR arrives past the overflow point.
        (&overlong_then_cr, true, true),
        // `\r` ends the line only with ICRNL.
        (&cr_at_limit, true, false),
        (&cr_at_limit, false, true),
    ];
    for (i, (bytes, cr_terminates, overflows)) in cases.iter().enumerate() {
        assert_eq!(
            exceeds_canonical_limit(bytes, 1024, *cr_terminates),
            *overflows,
            "case {i}"
        );
    }
}

/// The OS-default PTY (canonical mode) with the slave held open and
/// unread, so there is no foreground echo-back.
fn open_default_pty() -> (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::SlavePty + Send>,
) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    (pair.master, pair.slave)
}

/// The limit the guard resolves on this platform (1024 darwin, 4096
/// Linux); over-limit payloads are sized from it.
fn platform_canonical_limit() -> usize {
    let (master, _slave) = open_default_pty();
    let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
    // SAFETY: `raw_fd` names `master`, kept alive by the binding above
    // for the whole call; the borrow does not outlive this function and
    // is never used to close or duplicate the fd.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
    canonical_limit(borrowed)
}

/// A wire cwd is the directory the child starts in, before any `cd`.
#[test]
fn spawn_pty_starts_in_the_requested_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let expected = dir.path().canonicalize().expect("canonicalize");
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("pwd; read _");
    cmd.cwd(expected.as_os_str());
    let (mut pty_rx, _input_tx, mut pty) =
        spawn_pty(cmd, 80, 24, super::ORPHAN_DRAIN_BUDGET).expect("spawn");
    let pid = pty.child.process_id().expect("child pid");
    let immediate =
        phux_resource::cwd_query::process_cwd(pid).map(|path| path.canonicalize().unwrap_or(path));
    assert_eq!(
        immediate.as_deref(),
        Some(expected.as_path()),
        "cwd must be applied before posix_spawn returns, pid {pid}"
    );
    let mut received = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && !received.contains(&b'\n') {
        match pty_rx.try_recv() {
            Ok(PtyEvent::Bytes { chunk, .. }) => received.extend_from_slice(&chunk),
            Ok(PtyEvent::Eof) => break,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    let settled =
        phux_resource::cwd_query::process_cwd(pid).map(|path| path.canonicalize().unwrap_or(path));
    let _ = pty.child.kill();
    let printed = String::from_utf8_lossy(&received);
    assert_eq!(
        settled.as_deref(),
        Some(expected.as_path()),
        "pid {pid} immediate {immediate:?} printed {printed:?}"
    );
}

/// Spawn `cmd` under a real PTY and collect output up to the first `]`.
async fn spawn_and_capture(cmd: CommandBuilder) -> String {
    let (mut pty_rx, _input_tx, mut pty) =
        spawn_pty(cmd, 80, 24, super::ORPHAN_DRAIN_BUDGET).expect("spawn sh under a real pty");
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + DELIVERY_DEADLINE;
    while !received.contains(&b']') {
        match tokio::time::timeout_at(deadline, pty_rx.recv()).await {
            Ok(Some(PtyEvent::Bytes { chunk, .. })) => received.extend_from_slice(&chunk),
            Ok(Some(PtyEvent::Eof) | None) | Err(_) => break,
        }
    }
    let _ = pty.child.kill();
    String::from_utf8_lossy(&received).into_owned()
}

/// A pane child never inherits the server's `PHUX_UPGRADE_*` handoff.
#[tokio::test(flavor = "current_thread")]
async fn pane_children_never_inherit_the_upgrade_handoff_env() {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(
        "printf '[%s|%s|%s]' \"${PHUX_UPGRADE_SOURCE_EXE-unset}\" \
         \"${PHUX_UPGRADE_SNAPSHOT_DIR-unset}\" \"${PHUX_SOCKET-unset}\"",
    );
    for key in phux_resource::handoff_env::HANDOFF_ENV_VARS {
        cmd.env(key, "/leaked/by/an/upgraded/server");
    }
    cmd.env("PHUX_SOCKET", "/tmp/kept.sock");
    let out = spawn_and_capture(cmd).await;
    assert!(
        out.contains("[unset|unset|/tmp/kept.sock]"),
        "a pane child must see PHUX_SOCKET but no PHUX_UPGRADE_* variable; got {out:?}"
    );
}

/// A pane child never inherits Claude Code's nested-session markers from
/// the server's environment (a `claude` there would keep no transcript).
#[tokio::test(flavor = "current_thread")]
async fn pane_children_never_inherit_claude_code_nested_session_markers() {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(
        "printf '[%s|%s|%s|%s]' \"${CLAUDE_CODE_CHILD_SESSION-unset}\" \
         \"${CLAUDE_CODE_SESSION_ID-unset}\" \"${CLAUDECODE-unset}\" \
         \"${PHUX_SOCKET-unset}\"",
    );
    cmd.env("CLAUDE_CODE_CHILD_SESSION", "1");
    cmd.env("CLAUDE_CODE_SESSION_ID", "leaked-by-the-server-env");
    cmd.env("CLAUDECODE", "1");
    cmd.env("PHUX_SOCKET", "/tmp/kept.sock");
    let out = spawn_and_capture(cmd).await;
    assert!(
        out.contains("[unset|unset|unset|/tmp/kept.sock]"),
        "a pane child must see PHUX_SOCKET but no Claude Code marker; got {out:?}"
    );
}

/// A pane child never inherits Rex's nested-terminal resume token. A server
/// started inside a Rex pane carries `OSC7577_TOKEN`, and OSC 7577 says a
/// nested terminal must not pass that token on.
#[tokio::test(flavor = "current_thread")]
async fn pane_children_never_inherit_osc7577_token() {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("printf '[%s|%s]' \"${OSC7577_TOKEN-unset}\" \"${PHUX_SOCKET-unset}\"");
    cmd.env("OSC7577_TOKEN", "leaked-by-a-rex-pane");
    cmd.env("PHUX_SOCKET", "/tmp/kept.sock");
    let out = spawn_and_capture(cmd).await;
    assert!(
        out.contains("[unset|/tmp/kept.sock]"),
        "a pane child must see PHUX_SOCKET but no OSC7577_TOKEN; got {out:?}"
    );
}

/// `cat` under a real PTY: its reader channel, writer mailbox, and PTY.
fn spawn_cat() -> (
    tokio::sync::mpsc::Receiver<PtyEvent>,
    tokio::sync::mpsc::Sender<EncodedInputRequest>,
    PtyOwned,
) {
    let cmd = CommandBuilder::new("cat");
    spawn_pty(cmd, 80, 24, super::ORPHAN_DRAIN_BUDGET).expect("spawn cat under a real pty")
}

/// Send `bytes` to the writer thread and wait for its completion.
async fn write_and_wait(
    input_tx: &tokio::sync::mpsc::Sender<EncodedInputRequest>,
    bytes: Vec<u8>,
) -> WriteCompletion {
    let (completion_tx, completed) = WriteCompletionSink::channel();
    input_tx
        .try_send(EncodedInputRequest::acknowledged(bytes, completion_tx))
        .expect("writer mailbox has room");
    tokio::task::spawn_blocking(move || completed.recv_timeout(DELIVERY_DEADLINE))
        .await
        .expect("blocking task")
        .expect("writer thread must reply")
}

/// An overlong run plus terminating CR is refused whole instead of
/// wedging the pane.
#[tokio::test(flavor = "current_thread")]
async fn terminating_cr_past_the_limit_is_refused_not_wedged() {
    let (mut pty_rx, input_tx, mut pty) = spawn_cat();
    let mut payload = vec![b'a'; platform_canonical_limit() + 776];
    payload.push(b'\r');
    assert!(matches!(
        write_and_wait(&input_tx, payload).await,
        WriteCompletion::CanonicalLimitExceeded { .. }
    ));
    let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
    assert!(
        extra.is_err(),
        "a refused payload must not put any bytes on the wire"
    );
    let _ = pty.child.kill();
}

/// A refusal leaves the input path alive for later writes.
#[tokio::test(flavor = "current_thread")]
async fn refusal_does_not_wedge_the_pane_for_later_writes() {
    let (mut pty_rx, input_tx, mut pty) = spawn_cat();
    assert!(matches!(
        write_and_wait(&input_tx, vec![b'a'; 5000]).await,
        WriteCompletion::CanonicalLimitExceeded { .. }
    ));
    assert_eq!(
        write_and_wait(&input_tx, b"hello\n".to_vec()).await,
        WriteCompletion::Delivered
    );
    let got = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
        .await
        .expect("must not hang")
        .expect("pty output channel open");
    match got {
        PtyEvent::Bytes { chunk: bytes, .. } => {
            assert!(bytes.windows(5).any(|w| w == b"hello"));
        }
        PtyEvent::Eof => panic!("pane closed unexpectedly"),
    }
    let _ = pty.child.kill();
}

/// Raw mode stays on the unchecked path: a large newline-free payload is
/// delivered intact.
#[tokio::test(flavor = "current_thread")]
async fn raw_mode_delivers_a_large_newline_free_payload_intact() {
    let (mut pty_rx, input_tx, mut pty) = spawn_cat();
    // Raw mode (and no echo) on the shared master, as a TUI would set it.
    set_raw(&pty);

    let payload = vec![b'x'; 8192];
    assert_eq!(
        write_and_wait(&input_tx, payload.clone()).await,
        WriteCompletion::Delivered
    );

    let mut received = Vec::new();
    while received.len() < payload.len() {
        let chunk = tokio::time::timeout(DELIVERY_DEADLINE, pty_rx.recv())
            .await
            .expect("must not hang")
            .expect("pty output channel open");
        match chunk {
            PtyEvent::Bytes { chunk: bytes, .. } => received.extend_from_slice(&bytes),
            PtyEvent::Eof => panic!("pty closed before full delivery"),
        }
    }
    assert_eq!(received.len(), payload.len());
    assert_eq!(received, payload);
    let _ = pty.child.kill();
}

fn set_raw(pty: &PtyOwned) {
    let raw_fd = pty
        .master
        .lock()
        .expect("master lock")
        .as_raw_fd()
        .expect("raw fd");
    // SAFETY: `raw_fd` names `pty.master`, kept alive by `pty` for the
    // rest of the test; the borrow does not outlive this call.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
    let mut termios = nix::sys::termios::tcgetattr(borrowed).expect("tcgetattr");
    // A binary flood includes XOFF and INTR. Leaving `IXON` or `ISIG`
    // set would stop output or signal the child, and the writer would
    // block instead of the bytes coming back.
    termios.input_flags.remove(
        InputFlags::IGNBRK
            | InputFlags::BRKINT
            | InputFlags::PARMRK
            | InputFlags::ISTRIP
            | InputFlags::INLCR
            | InputFlags::IGNCR
            | InputFlags::ICRNL
            | InputFlags::IXON
            | InputFlags::IXOFF,
    );
    termios.output_flags.remove(OutputFlags::OPOST);
    termios.local_flags.remove(
        LocalFlags::ECHO
            | LocalFlags::ECHONL
            | LocalFlags::ICANON
            | LocalFlags::ISIG
            | LocalFlags::IEXTEN,
    );
    termios.control_chars[nix::sys::termios::SpecialCharacterIndices::VMIN as usize] = 1;
    termios.control_chars[nix::sys::termios::SpecialCharacterIndices::VTIME as usize] = 0;
    nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &termios)
        .expect("tcsetattr");
}

mod reader;
