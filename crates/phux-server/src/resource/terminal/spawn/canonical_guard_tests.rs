use super::*;
use crate::terminal_actor::WriteCompletionSink;
use nix::sys::termios::OutputFlags;
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

/// Mechanism proof: `write_all_resilient` alone lets the kernel truncate
/// an overlong canonical line, which is why the guard sits in front of it.
///
/// It measures what the pane's reader receives on the slave side, not
/// the master's echo: Linux echoes every byte of an overflowing line
/// (each one overwrites the last queue slot), so how much echo one read
/// returned depended on scheduling, and hosted CI once read 4120 bytes.
#[test]
fn write_all_resilient_alone_truncates_a_canonical_mode_line() {
    // Linux truncates the line and delivers it at the newline; macOS
    // drops the overflow and the newline with it, so nothing arrives.
    const LINE_WAIT: Duration = if cfg!(target_os = "linux") {
        DELIVERY_DEADLINE
    } else {
        NOTHING_ARRIVES_WINDOW
    };
    let (master, _slave) = open_default_pty();
    let slave_path = master.tty_name().expect("real pty names its slave");
    let slave_reader = std::fs::File::from(
        nix::fcntl::open(
            &slave_path,
            nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_NOCTTY,
            nix::sys::stat::Mode::empty(),
        )
        .expect("open the slave for reading"),
    );
    let mut writer = master.take_writer().expect("take writer");
    let raw_fd = master.as_raw_fd().expect("real pty has a raw fd");
    // SAFETY: `raw_fd` names `master`, which this test keeps alive via
    // the `master` binding for the whole function; the borrow does not
    // outlive this synchronous call.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
    let limit = canonical_limit(borrowed);

    // The pane's side: read until the line completes or the pty closes.
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    let slave_thread = std::thread::spawn(move || {
        let mut slave_reader = slave_reader;
        let mut line = Vec::new();
        let mut buf = [0u8; 8192];
        while !line.contains(&b'\n') {
            match slave_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => line.extend_from_slice(&buf[..n]),
            }
        }
        let _ = line_tx.send(line);
    });

    // Oversized line plus a terminator so a truncated line is readable.
    let payload = vec![b'a'; limit + 37];
    write_all_resilient(&mut *writer, &payload)
        .expect("the kernel write(2) itself succeeds regardless");
    write_all_resilient(&mut *writer, b"\n").expect("terminator write succeeds");
    flush_resilient(&mut *writer).expect("flush");

    let line = line_rx.recv_timeout(LINE_WAIT).unwrap_or_default();
    // Hanging up the master ends a slave read still waiting on a line.
    drop(writer);
    drop(master);
    slave_thread.join().expect("slave reader thread");
    assert!(
        line.len() <= limit,
        "expected at most {limit} bytes (the canonical limit) to reach \
             the reader, got {}: the canonical queue did not overflow, so \
             this payload was too small to reproduce phux-mjmc on this \
             platform",
        line.len()
    );
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
    let (mut pty_rx, _input_tx, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn");
    let pid = pty.child.process_id().expect("child pid");
    let immediate =
        crate::cwd_query::process_cwd(pid).map(|path| path.canonicalize().unwrap_or(path));
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
        crate::cwd_query::process_cwd(pid).map(|path| path.canonicalize().unwrap_or(path));
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
        spawn_pty(cmd, 80, 24).expect("spawn sh under a real pty");
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
    for key in crate::upgrade::HANDOFF_ENV_VARS {
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

mod readers;
mod write_limits;
