use super::*;
/// The actor answers `PwdRequest` with the PTY child's live cwd.
#[tokio::test(flavor = "current_thread")]
async fn actor_responds_to_pwd_request_with_pty_child_cwd() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            // macOS returns the realpath, like the kernel query.
            let dir_path = dir.path().canonicalize().expect("canonicalize tempdir");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("cd '{}' && read _", dir_path.display()));
            let bundle = TerminalActor::build_with_token(
                20,
                5,
                Some(cmd),
                DEFAULT_SCROLLBACK,
                CancellationToken::new(),
            )
            .expect("build_with_token");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            // The query races the shell's `cd`; retry briefly.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut got: Option<String> = None;
            while tokio::time::Instant::now() < deadline {
                let (reply_tx, reply_rx) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .pwd
                    .send(PwdRequest { reply: reply_tx })
                    .await
                    .expect("send pwd request");
                got = reply_rx.await.expect("pwd reply");
                if got.as_deref() == Some(dir_path.to_str().expect("utf8 path")) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            assert_eq!(
                got.as_deref(),
                Some(dir_path.to_str().expect("utf8 path")),
                "actor should report the PTY child's live CWD",
            );

            token.cancel();
            let _ = tokio::time::timeout(ACTOR_EXIT_DEADLINE, join).await;
        })
        .await;
}

/// A PTY-less actor answers `pwd` with `None`.
#[tokio::test(flavor = "current_thread")]
async fn actor_pwd_request_is_none_without_pty() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(20, 5, b"no pty here").expect("seed");
            let handle = bundle.handle.clone();
            let _token = bundle.token;
            tokio::task::spawn_local(bundle.actor.run());

            let (reply_tx, reply_rx) = oneshot::channel();
            handle
                .terminal()
                .expect("terminal facet")
                .pwd
                .send(PwdRequest { reply: reply_tx })
                .await
                .expect("send pwd request");
            assert_eq!(reply_rx.await.expect("pwd reply"), None);
        })
        .await;
}

/// The actor stops promptly on cancellation, channels open or not.
#[tokio::test(flavor = "current_thread")]
async fn actor_exits_on_cancellation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new(20, 5).expect("new");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");

            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = handle
                .terminal()
                .expect("terminal facet")
                .snapshot
                .try_send(SnapshotRequest {
                    image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
                    scrollback: None,
                    max_bytes: usize::MAX,
                    max_frames: usize::MAX,
                    chunk_bytes: 1,
                    reply: reply_tx,
                });
            drop(reply_rx);
        })
        .await;
}

/// Cancelling a parent token stops a child-token actor.
#[tokio::test(flavor = "current_thread")]
async fn parent_token_cancel_cascades_to_pane_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let parent = CancellationToken::new();
            let child = parent.child_token();
            let bundle = TerminalActor::build_with_token(20, 5, None, DEFAULT_SCROLLBACK, child)
                .expect("build_with_token");
            let join = tokio::task::spawn_local(bundle.actor.run());

            parent.cancel();

            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after its parent token was cancelled")
                .expect("actor task panicked");
        })
        .await;
}
