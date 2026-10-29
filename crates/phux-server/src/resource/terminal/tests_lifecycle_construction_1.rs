use super::*;
#[test]
fn seeded_default_colors_are_installed_before_actor_run() {
    use phux_protocol::caps::{TerminalColor, TerminalDefaultColors};

    let colors = TerminalDefaultColors {
        foreground: TerminalColor {
            r: 208,
            g: 208,
            b: 208,
        },
        background: TerminalColor {
            r: 18,
            g: 24,
            b: 27,
        },
    };
    let bundle = TerminalActor::build_with_token_and_colors(
        80,
        24,
        None,
        test_scrollback(100),
        CancellationToken::new(),
        Some(colors),
    )
    .expect("actor");
    let mut actor = bundle.actor;
    {
        let canonical = actor.terminal.borrow();
        let terminal = canonical.try_terminal().expect("no capture in flight");
        assert_eq!(
            terminal.default_fg_color().expect("foreground"),
            Some(libghostty_vt::style::RgbColor {
                r: 208,
                g: 208,
                b: 208,
            })
        );
        assert_eq!(
            terminal.default_bg_color().expect("background"),
            Some(libghostty_vt::style::RgbColor {
                r: 18,
                g: 24,
                b: 27,
            })
        );
    }

    let (_pty_output, mut pty_input) = actor.install_test_pty_channels();
    let first = b"\x1b]10;?\x1b";
    let second = b"\\\x1b]11;?\x1b\\";
    actor.terminal.borrow_mut().vt_write(first);
    actor.terminal.borrow_mut().vt_write(second);
    assert_eq!(
        pty_input.try_recv().expect("OSC 10 reply").bytes.as_ref(),
        b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\"
    );
    assert_eq!(
        pty_input.try_recv().expect("OSC 11 reply").bytes.as_ref(),
        b"\x1b]11;rgb:1212/1818/1b1b\x1b\\"
    );
    assert!(pty_input.try_recv().is_err(), "each query is answered once");
}

#[test]
fn ask_marker_parse() {
    for title in ["", "vim README.md", "phux-ask", "phux-ask[q1]"] {
        assert_eq!(AskMarker::parse(title), None, "{title:?}");
    }
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("phux-ask:Proceed?", "", "Proceed?", &[]),
        (
            "phux-ask[q1]:Deploy to prod??s=Yes|No|Hold",
            "q1",
            "Deploy to prod?",
            &["Yes", "No", "Hold"],
        ),
        ("phux-ask:Ready??s=", "", "Ready?", &[]),
        ("phux-ask:Pick??s=a||b", "", "Pick?", &["a", "b"]),
    ];
    for (title, id, question, suggestions) in cases {
        let marker = AskMarker::parse(title).expect("a phux-ask marker");
        assert_eq!(marker.id, *id, "{title}");
        assert_eq!(marker.question, *question, "{title}");
        assert_eq!(marker.suggestions, *suggestions, "{title}");
    }
}

/// A blank pane's snapshot opens with the reset preamble.
#[test]
fn synthesize_blank_pane_returns_reset_preamble() {
    let bundle = TerminalActor::new(80, 24).expect("new");
    let snap = bundle
        .actor
        .synthesize_with_scrollback(None)
        .expect("synthesize");
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.rows, 24);
    assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
}

/// Seed bytes reach the synthesized snapshot.
#[test]
fn synthesize_seeded_pane_carries_visible_text() {
    let bundle = TerminalActor::new_with_seed(20, 5, b"hello").expect("new_with_seed");
    let snap = bundle
        .actor
        .synthesize_with_scrollback(None)
        .expect("synthesize");
    let body = String::from_utf8_lossy(&snap.bytes);
    assert!(
        body.contains("hello"),
        "synthesized bytes should contain seeded text, got: {body:?}"
    );
}

/// Sustained PTY readiness must leave a bounded slot for ordinary requests.
#[tokio::test(flavor = "current_thread")]
async fn snapshot_runs_after_one_bounded_pty_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for panes in [1, 8, 32] {
                futures_util::future::join_all((0..panes).map(|_| snapshot_under_pty_pressure(0)))
                    .await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_is_not_starved_by_a_refilling_pty_producer() {
    tokio::task::LocalSet::new()
        .run_until(snapshot_under_pty_pressure(1024))
        .await;
}

/// The actor answers `SnapshotRequest` with what the synthesizer produces.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_snapshot_request_on_localset() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"hi there").expect("new_with_seed");
            let handle = bundle.handle.clone();
            // Dropping the token does not cancel; keep it anyway.
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .snapshot
                .send(SnapshotRequest {
                    image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
                    scrollback: None,
                    max_bytes: usize::MAX,
                    max_frames: usize::MAX,
                    chunk_bytes: 1,
                    reply: reply_tx,
                })
                .await
                .expect("send snapshot request");
            let (snap, base_seq) = reply_rx
                .await
                .expect("snapshot reply")
                .expect("snapshot synthesis");
            assert_eq!(snap.cols, 20);
            assert_eq!(snap.rows, 5);
            assert_eq!(base_seq, 0);
            let body = String::from_utf8_lossy(&snap.bytes);
            assert!(
                body.contains("hi there"),
                "actor-synthesized bytes should contain seeded text"
            );
        })
        .await;
}

/// A PTY-less actor's upgrade handle has a snapshot but no descriptors.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_handle_no_pty_has_snapshot_but_no_descriptors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"seeded").expect("new_with_seed");
            let handle = bundle.handle.clone();
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .upgrade
                .send(UpgradeHandleRequest { reply: reply_tx })
                .await
                .expect("send upgrade request");
            let h = reply_rx.await.expect("upgrade reply");
            assert!(h.master_fd.is_none());
            assert_eq!(h.child_pid, None);
            assert_eq!((h.cols, h.rows), (20, 5));
            assert!(
                String::from_utf8_lossy(&h.vt_replay_bytes).contains("seeded"),
                "replay snapshot should carry the seeded text"
            );
        })
        .await;
}

/// A PTY-backed actor's upgrade handle carries the master fd and child pid.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_handle_with_pty_exposes_fd_and_pid() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut cmd = portable_pty::CommandBuilder::new("sleep");
            cmd.arg("30");
            let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("new_with_command");
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .upgrade
                .send(UpgradeHandleRequest { reply: reply_tx })
                .await
                .expect("send upgrade request");
            let h = reply_rx.await.expect("upgrade reply");
            assert!(h.master_fd.is_some(), "PTY actor should expose a master fd");
            assert!(h.child_pid.is_some(), "PTY actor should expose a child pid");

            // Cancel so the actor reaps the `sleep` child.
            token.cancel();
        })
        .await;
}
