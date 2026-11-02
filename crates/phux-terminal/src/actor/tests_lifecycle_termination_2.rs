use super::*;

/// A child that refuses to die cannot hang the server: the spewer escapes
/// into its own process group (unreachable by the signalling path), so
/// teardown finishes because it is bounded. The reap budget itself is
/// covered by `teardown_policy_tests`.
#[tokio::test(flavor = "current_thread")]
async fn pane_kill_is_bounded_when_an_escaped_child_ignores_the_hangup_and_spews() {
    use portable_pty::CommandBuilder;

    let ceiling = PANE_KILL_GRACE * 8;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let armed = dir.path().join("armed");
            let payload = dir.path().join("payload");
            let foreground = FixtureGroup::new(dir.path(), "PHUX_TEST_FOREGROUND");
            let holder_cleanup = FixtureGroup::new(dir.path(), "PHUX_TEST_HOLDER");
            std::fs::write(&payload, vec![b'.'; 256 * 1024]).expect("write payload");
            let spewer = dir.path().join("holder.sh");
            std::fs::write(
                &spewer,
                holder_cleanup
                    .script("trap '' HUP\nwhile :; do cat \"$PHUX_TEST_PAYLOAD\" || exit; done\n"),
            )
            .expect("write holder script");

            // `trap '' HUP` survives `exec`; `set -m` puts the loop in its
            // own group. It ignores hangup, never sees the hard kill, and
            // writes nonstop.
            let script = dir.path().join("foreground.sh");
            std::fs::write(
                &script,
                foreground.script(
                    "trap '' HUP\n\
                     set -m\n\
                     /bin/sh \"$PHUX_TEST_SPEWER\" &\n\
                     while [ ! -s \"$PHUX_TEST_HOLDER\" ]; do /bin/sleep 0.01; done\n\
                     printf armed > \"$PHUX_TEST_ARMED\"\n\
                     wait\n",
                ),
            )
            .expect("write foreground script");

            let mut cmd = CommandBuilder::new("/bin/sh");
            cmd.arg("-c");
            cmd.arg(format!("set -m; trap '' HUP; /bin/sh {}", script.display()));
            cmd.env("PHUX_TEST_ARMED", &armed);
            cmd.env("PHUX_TEST_PAYLOAD", &payload);
            cmd.env("PHUX_TEST_SPEWER", &spewer);
            foreground.configure(&mut cmd);
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

            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while !armed.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect(
                "the fixture shell never started: an environment problem (machine load), \
                     not a failure of the path this test covers",
            );

            let killed_at = std::time::Instant::now();
            token.cancel();
            tokio::time::timeout(ceiling, run)
                .await
                .expect(
                    "pane teardown never completed for a child that escaped the signalled \
                     groups and keeps writing: the actor is blocked, and on a shared \
                     current-thread runtime every pane is blocked with it",
                )
                .expect("actor task failed");
            let shutdown_took = killed_at.elapsed();

            assert!(
                shutdown_took < ceiling,
                "teardown must be bounded by the grace, reap and join budgets, not by the \
                     child's willingness to exit; took {shutdown_took:?}",
            );
            // `armed` is written only after the holder pid is on record.
        })
        .await;
}
