//! An ensure must not leak the caller's descriptors into the daemon it
//! starts, nor start more than one daemon when callers race.

use std::io::Read as _;

use super::{Fixture, assert_success, bounded_output};

#[test]
fn concurrent_ensures_elect_one_spawner() {
    let mut fixture = Fixture::new();
    let mut commands: Vec<_> = (0..4).map(|_| fixture.command()).collect();
    let outputs = std::thread::scope(|scope| {
        let workers: Vec<_> = commands
            .iter_mut()
            .map(|cmd| scope.spawn(|| bounded_output(cmd.args(["server", "--ensure"]))))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("ensure worker"))
            .collect::<Vec<_>>()
    });
    // Worker failures first: they explain a missing server better than the
    // capture timeout would, and the fixture reaps a live socket regardless.
    for output in &outputs {
        assert_success(output);
    }
    fixture.server.capture_pid();
    let log = std::fs::read_to_string(fixture.dir.path().join("phux-ensure/server.log"))
        .expect("daemon log");
    assert_eq!(
        log.matches("phux: info: server listening on").count(),
        1,
        "{log}"
    );
    fixture.assert_cleaned_up();
}

/// The daemon an ensure starts outlives it, so it must not inherit a stray
/// descriptor of the caller's: a caller reading to EOF would wait on the
/// server instead of the ensure. On macOS a multi-threaded caller leaks sibling
/// capture pipes exactly this way (`Command` sets `FD_CLOEXEC` after `pipe()`),
/// which is how `concurrent_ensures_elect_one_spawner` blocked until the
/// server's idle exit and then found no server (phux-5wxp.5).
///
/// The inheritable writer leaks into whatever a sibling test spawns at the
/// same moment, so under `cargo test`'s shared process this re-runs itself
/// alone; nextest already gives every test its own process.
#[test]
fn the_spawned_daemon_does_not_hold_the_callers_descriptors() {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    const SOLO: &str = "PHUX_TEST_SOLO_PROCESS";
    if std::env::var_os("NEXTEST").is_none() && std::env::var_os(SOLO).is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "server_ensure::descriptors::the_spawned_daemon_does_not_hold_the_callers_descriptors",
                "--test-threads=1",
            ])
            .env(SOLO, "1")
            .status()
            .expect("re-run the test in its own process");
        assert!(status.success(), "the solo run failed: {status}");
        return;
    }
    let mut fixture = Fixture::new();
    let (mut reader, writer) = std::io::pipe().expect("pipe");
    let flags = fcntl_getfd(&writer).expect("descriptor flags");
    fcntl_setfd(&writer, flags.difference(FdFlags::CLOEXEC)).expect("inheritable writer");

    let output = bounded_output(fixture.command().args(["server", "--ensure"]));
    // Before anything else is spawned: `status` must not inherit it either.
    drop(writer);
    fixture.server.capture_pid();
    assert_success(&output);

    // The ensure has exited and our end is closed; the daemon is the only
    // process that could still hold the writer. No timing: a held writer
    // reads as `WouldBlock`, a released one as EOF.
    let status_flags = rustix::fs::fcntl_getfl(&reader).expect("status flags");
    rustix::fs::fcntl_setfl(&reader, status_flags | rustix::fs::OFlags::NONBLOCK)
        .expect("nonblocking reader");
    let mut byte = [0_u8; 1];
    match reader.read(&mut byte) {
        Ok(0) => {}
        other => panic!("the daemon kept the caller's pipe open: {other:?}"),
    }
    fixture.assert_cleaned_up();
}
