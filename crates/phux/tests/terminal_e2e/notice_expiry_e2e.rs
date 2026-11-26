//! A status-bar notice goes away on its own while a pane keeps printing: the
//! bar's one-second tick must not be put off by output that comes faster.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use crate::common;

use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(20);
/// Past the notice's seven seconds, with room for a loaded machine.
const EXPIRY: Duration = Duration::from_secs(15);
const NOTICE: &str = "typing goes to the focused pane only";

#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn a_notice_expires_while_a_pane_keeps_printing() {
    let server = common::ServerGuard::builder("notice")
        .env("SHELL", "/bin/sh")
        .seed_command("while :; do date; sleep 0.05; done")
        .start();
    let mut client = common::PtyAttach::start(&server.socket, &[], (120, 40), &[]);
    client.await_painted(&["+ session"], DEADLINE);
    client.send(b"\x01y");
    client.await_painted(&["typing now goes to every pane"], DEADLINE);
    client.send(b"\x01y");
    client.await_painted(&[NOTICE], DEADLINE);

    // The notice paints over the row; its expiry paints the row again.
    let deadline = Instant::now() + EXPIRY;
    loop {
        let painted = client.painted();
        let (_, after) = painted.rsplit_once(NOTICE).expect("the notice");
        if after.contains("+ session") {
            break;
        }
        assert!(Instant::now() < deadline, "the notice never cleared");
        std::thread::sleep(Duration::from_millis(100));
    }
}
