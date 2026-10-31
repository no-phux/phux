use super::*;

/// Killing a pane gives a foreground job in its own process group a chance
/// to flush before it dies. The hangup ceiling is stretched for load;
/// nextest gives this test all CPUs.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_lets_foreground_process_flush_before_death() {
    use portable_pty::CommandBuilder;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("flushed");
            let armed = dir.path().join("armed");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");

            // The script signals once its HUP trap is installed; a SIGHUP
            // before `trap` would hit the default disposition.
            let script = dir.path().join("foreground.sh");
            // A builtin wait lets HUP run the trap directly.
            std::fs::write(
                &script,
                foreground.script(
                    "trap 'printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do read _; done\n",
                ),
            )
            .expect("write foreground script");

            // Monitor mode: the script is a foreground job in its own group.
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!(
                "set -m; trap ':' HUP; /bin/sh {}",
                script.display()
            ));
            cmd.env("PHUX_TEST_MARKER", &marker);
            cmd.env("PHUX_TEST_ARMED", &armed);
            foreground.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                20,
                5,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let pty = actor.pty.as_ref().expect("test actor has PTY");
            let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                .expect("shell pid fits i32");
            let _pane_cleanup = FixturePane(shell_group);
            let master = std::sync::Arc::clone(&pty.master);
            let run = tokio::task::spawn_local(actor.run());

            // One barrier: `armed` is written after `trap` by the process
            // `set -m` put in its own group, so it implies both
            // preconditions. The generous budget covers process startup
            // under load, not the subject under test.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !armed.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "foreground job never installed its SIGHUP trap: the fixture shells did not \
                     get scheduled, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let _grace = stretch_pane_kill_grace(CONTENDED_FLUSH_GRACE);

            // Armed, so the foreground group is settled.
            let foreground_group = master
                .lock()
                .expect("master lock")
                .process_group_leader()
                .expect("an armed foreground job has a process group");
            assert_ne!(
                foreground_group, shell_group,
                "the fixture must reproduce interactive job-control topology: a foreground \
                     job in a group distinct from the shell's",
            );

            // Kill the pane. The actor's shutdown runs SIGHUP + grace.
            token.cancel();
            let body = wait_for_flush_marker(&marker).await;
            tokio::time::timeout(std::time::Duration::from_secs(5), run)
                .await
                .expect("actor shutdown timed out")
                .expect("actor task failed");

            assert!(
                body.contains("flushed"),
                "foreground process must run its SIGHUP flush handler before \
                     the pane is killed; marker={body:?}",
            );
        })
        .await;
}

/// The hangup grace lets a foreground job finish flushing to the terminal:
/// the reader must keep draining during teardown, or the child wedges in
/// `write(2)` and is hard-killed before the marker appears.
///
/// Fixture requirements: the outer shell survives SIGHUP (a dying session
/// leader revokes the tty, masking the bug); the trap masks further hangups
/// (so `cat` survives the second SIGHUP); and `&&` so the marker means
/// every byte arrived.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_lets_a_terminal_flush_finish_inside_the_grace() {
    use portable_pty::CommandBuilder;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("flushed");
            let started = dir.path().join("started");
            let armed = dir.path().join("armed");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");
            let payload = dir.path().join("payload");
            let status = dir.path().join("status");
            let stderr = dir.path().join("err");
            std::fs::write(&payload, vec![b'.'; TERMINAL_FLUSH_BYTES]).expect("write flush payload");

            // `cat` moves megabytes within the 500 ms budget; a builtin
            // `read` lets HUP run the trap at once.
            let script = dir.path().join("foreground.sh");
            std::fs::write(
                &script,
                foreground.script(
                    "trap 'trap \"\" HUP; printf flushing > \"$PHUX_TEST_STARTED\"; \
                     cat \"$PHUX_TEST_PAYLOAD\" 2>\"$PHUX_TEST_ERR\"; s=$?; \
                     printf %s \"$s\" > \"$PHUX_TEST_STATUS\"; \
                     [ \"$s\" -eq 0 ] && printf flushed > \"$PHUX_TEST_MARKER\"; exit 0' HUP\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     while :; do read _; done\n",
                ),
            )
            .expect("write foreground script");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            // A caught HUP on the session leader resets across `exec`, so the
            // inner shell installs its own trap while this shell keeps the
            // tty alive.
            cmd.arg(format!(
                "set -m; trap ':' HUP; /bin/sh {}",
                script.display()
            ));
            cmd.env("PHUX_TEST_MARKER", &marker);
            cmd.env("PHUX_TEST_STARTED", &started);
            cmd.env("PHUX_TEST_ARMED", &armed);
            cmd.env("PHUX_TEST_PAYLOAD", &payload);
            cmd.env("PHUX_TEST_STATUS", &status);
            cmd.env("PHUX_TEST_ERR", &stderr);
            foreground.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                80,
                24,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let pty = actor.pty.as_ref().expect("test actor has PTY");
            let shell_group = i32::try_from(pty.child.process_id().expect("shell pid"))
                .expect("shell pid fits i32");
            let _pane_cleanup = FixturePane(shell_group);
            let master = std::sync::Arc::clone(&pty.master);
            let run = tokio::task::spawn_local(actor.run());

            assert!(
                wait_until_fixture_exists(&armed).await,
                "foreground job never installed its SIGHUP trap: the fixture shells did not \
                     get scheduled, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let _grace = stretch_pane_kill_grace_after(CONTENDED_FLUSH_GRACE, Some(&started));

            let foreground_group = master
                .lock()
                .expect("master lock")
                .process_group_leader()
                .expect("an armed foreground job has a process group");
            assert_ne!(
                foreground_group, shell_group,
                "the fixture must reproduce interactive job-control topology: a foreground \
                     job in a group distinct from the shell's",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            assert!(
                wait_until_fixture_exists(&started).await,
                "SIGHUP trap never started: the fixture shell did not get scheduled \
                     after hangup, which is an environment problem (machine load), not a \
                     failure of the flush-before-death path this test covers",
            );
            let body = wait_for_flush_marker(&marker).await;
            tokio::time::timeout(std::time::Duration::from_secs(10), run)
                .await
                .expect("actor shutdown stalled: the pane-kill path did not complete")
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();
            let cat_status = std::fs::read_to_string(&status).unwrap_or_default();
            let cat_err = std::fs::read_to_string(&stderr).unwrap_or_default();
            assert!(
                body.contains("flushed"),
                "a foreground job flushing {TERMINAL_FLUSH_BYTES} bytes to the TERMINAL must finish \
                     inside the hangup grace. An empty marker with a non-zero cat status means \
                     it could not write: either it blocked against an undrained PTY and was \
                     hard-killed mid-flush, or the terminal was revoked out from under it. \
                     marker={body:?} cat_status={cat_status:?} cat_err={cat_err:?} \
                     shutdown_took={shutdown_took:?}",
            );
        })
        .await;
}

/// A process that escaped the snapshotted groups and holds the slave open
/// cannot hang the server: teardown drops the PTY before a bounded join.
///
/// On macOS this passes even without the fix (the kernel revokes the tty
/// when the session leader exits); on Linux it is a real gate. The
/// kernel-independent property is pinned by
/// `teardown_policy_tests::a_thread_that_never_exits_is_detached_rather_than_joined`.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_is_bounded_when_a_detached_process_holds_the_slave_open() {
    use portable_pty::CommandBuilder;

    let ceiling = PANE_KILL_GRACE * 8;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let holder_cleanup = FixtureGroup::new(dir.path(), "PHUX_TEST_HOLDER");
            let holder = holder_cleanup.pid_file().to_owned();
            let script = dir.path().join("holder.sh");
            // No `exec`: the holder shell keeps naming this tempdir for
            // cleanup. Shell and sleep both hold the slave open.
            std::fs::write(&script, holder_cleanup.script("/bin/sleep 3600\n"))
                .expect("write holder script");
            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            // The pane's own child dies to the hangup, leaving only the holder.
            cmd.arg(format!("set -m; /bin/sh {} & exec cat", script.display()));
            holder_cleanup.configure(&mut cmd);

            let token = CancellationToken::new();
            let bundle = TerminalActor::build_with_token(
                80,
                24,
                Some(cmd),
                test_scrollback(1000),
                token.clone(),
            )
            .expect("build actor");
            let actor = bundle.actor;
            let _pane_cleanup = FixturePane::for_actor(&actor);
            let run = tokio::task::spawn_local(actor.run());

            // Wait for the holder's pid file.
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !holder.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "the fixture never forked its detached holder: an environment problem \
                     (machine load), not a failure of the path this test covers",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            tokio::time::timeout(ceiling, run)
                .await
                .expect(
                    "pane teardown never completed while a detached process held the slave \
                     open: the actor is blocked joining a reader parked in read(2), and on a \
                     shared current-thread runtime that is every pane on the server",
                )
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();

            assert!(
                shutdown_took < ceiling,
                "teardown must be bounded by our own budgets, not by whether some process \
                     we never signalled decides to close the slave; took {shutdown_took:?}",
            );
        })
        .await;
}
