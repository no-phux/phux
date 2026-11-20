//! Stop grace begins only after both grandchildren have armed their traps.

use std::time::{Duration, Instant};

use phux_server_testkit::run_local;
use tempfile::TempDir;

use super::{Running, alive, catalog, dir, read_pid, sh, spec, timing_lock, wait_file};

/// The leaders exit on `SIGTERM` immediately. One grandchild handles
/// `SIGTERM` and must be allowed to finish; another ignores it and is
/// killed only after the shared deadline. The stop stays inside that
/// deadline.
#[test]
fn stop_grace_covers_a_grandchild_after_the_leader_exits() {
    let _timing = timing_lock();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let honor = dir(tmp.path(), "honor");
        let ignore = dir(tmp.path(), "ignore");
        let grandchild = |trap: &str| {
            format!("trap 'exit 0' TERM\n{trap} &\necho $! > grand\necho $$ > leader\nwait\n")
        };
        let honor_cmd = sh(
            &honor,
            &grandchild(
                "(trap 'sleep 1; touch done; exit 0' TERM; touch ready; while :; do :; done)",
            ),
        );
        let ignore_cmd = sh(
            &ignore,
            &grandchild("(trap '' TERM; touch ready; sleep 1000)"),
        );
        let server = Running::boot(
            catalog(vec![
                spec(&honor, "plug.honor", honor_cmd, 1),
                spec(&ignore, "plug.ignore", ignore_cmd, 2),
            ]),
            None,
            None,
        );
        wait_file(&honor.join("grand")).await;
        wait_file(&ignore.join("grand")).await;
        // The parent publishes PIDs before the children have armed their traps.
        wait_file(&honor.join("ready")).await;
        wait_file(&ignore.join("ready")).await;
        let ignore_grand = read_pid(&ignore.join("grand"));
        let started = Instant::now();
        server.stop().await;
        let elapsed = started.elapsed();
        assert!(
            honor.join("done").is_file(),
            "a TERM-honoring grandchild must finish during the shared grace"
        );
        assert!(
            !alive(ignore_grand),
            "a TERM-ignoring grandchild must be killed after the deadline"
        );
        assert!(
            elapsed > Duration::from_secs(4) && elapsed < Duration::from_secs(10),
            "stop took {elapsed:?}; the shared deadline is 5s"
        );
    });
}
