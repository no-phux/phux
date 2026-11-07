//! Write limits: extracted from the shared module.
use super::*;

/// A newline-free overlong write is refused before any byte is sent.
#[tokio::test(flavor = "current_thread")]
pub(super) async fn newline_free_write_over_the_limit_is_refused_before_any_byte_is_sent() {
    let (mut pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
    match write_and_wait(&input_tx, vec![b'a'; 4097]).await {
        WriteCompletion::CanonicalLimitExceeded { limit } => assert!(limit > 0),
        other => panic!("expected CanonicalLimitExceeded, got {other:?}"),
    }
    let extra = tokio::time::timeout(NOTHING_ARRIVES_WINDOW, pty_rx.recv()).await;
    assert!(
        extra.is_err(),
        "a refused payload must not put any bytes on the wire"
    );
    let _ = pty.child.kill();
}

/// An overlong run plus terminating CR is refused whole instead of
/// wedging the pane.
#[tokio::test(flavor = "current_thread")]
pub(super) async fn terminating_cr_past_the_limit_is_refused_not_wedged() {
    let (mut pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
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
pub(super) async fn refusal_does_not_wedge_the_pane_for_later_writes() {
    let (mut pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
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
pub(super) async fn raw_mode_delivers_a_large_newline_free_payload_intact() {
    let (mut pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
    // Raw mode (and no echo) on the shared master, as a TUI would set it.
    {
        let raw_fd = pty
            .master
            .lock()
            .expect("master lock")
            .as_raw_fd()
            .expect("raw fd");
        // SAFETY: `raw_fd` names `pty.master`, kept alive by `pty` for
        // the rest of this test; the borrow does not outlive this call.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        let mut termios = nix::sys::termios::tcgetattr(borrowed).expect("tcgetattr");
        termios
            .local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO);
        nix::sys::termios::tcsetattr(borrowed, nix::sys::termios::SetArg::TCSANOW, &termios)
            .expect("tcsetattr");
    }

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
