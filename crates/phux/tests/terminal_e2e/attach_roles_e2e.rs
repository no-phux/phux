//! Binary-level acceptance for attach roles (ADR-0127): a real `phux server`
//! and real `phux attach` TUIs in pseudoterminals. `--viewer` watches and its
//! typing never reaches the pane; `--take` attaches holding the input lease
//! and its typing does. Each typed line is `echo NAME_$((6*7))`, so `NAME_42`
//! appears on the screen only if the pane's shell executed it; the echoed
//! command line alone never contains it.
//!
//! `#[ignore]`: it spawns a real server and PTY clients. Run via `just e2e`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::ResourceInfo;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const SESSION: &str = "work";
const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
/// How long refused keystrokes get to prove they went nowhere.
const SETTLE: Duration = Duration::from_secs(2);

/// A running `phux server`, killed and unlinked when the guard drops.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    fn start() -> Self {
        Self(
            common::ServerGuard::builder("roles")
                .env("SHELL", "/bin/sh")
                .start(),
        )
    }

    /// The seed pane's screen text, as `phux snapshot` renders it.
    fn screen(&self) -> String {
        let out = common::phux_cmd(PHUX)
            .args(["snapshot", "--socket"])
            .arg(&self.socket)
            .arg(SESSION)
            .stdin(Stdio::null())
            .output()
            .expect("run phux snapshot");
        assert!(out.status.success(), "phux snapshot failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The seed pane's inventory row: its input holder and its viewers.
    fn pane(&self) -> ResourceInfo {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(pane_state(&self.socket))
    }
}

async fn pane_state(socket: &Path) -> ResourceInfo {
    let mut conn = tokio::time::timeout(DEADLINE, Connection::connect(socket))
        .await
        .expect("connect in time")
        .expect("HELLO");
    let result = tokio::time::timeout(
        DEADLINE,
        conn.request(
            1,
            WireCommand::GetState {
                scope: StateScope::Server,
            },
        ),
    )
    .await
    .expect("answered in time")
    .expect("request")
    .into_result_ignoring_interleaved();
    drop(conn);
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("GET_STATE failed: {result:?}");
    };
    snapshot
        .resources
        .into_iter()
        .find(|info| info.kind.is_terminal())
        .expect("the seed pane is in the inventory")
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(POLL);
    }
}

/// A real `phux attach` in a pseudoterminal, killed on drop.
#[test]
#[ignore = "spawns a real phux server and attached PTY clients; run via `just e2e`."]
fn attach_take_seizes_and_attach_viewer_cannot_type() {
    let server = ServerGuard::start();

    let mut viewer =
        common::PtyAttach::start(&server.socket, &["--viewer", SESSION], (110, 30), &[]);
    wait_until("the viewer is listed", || !server.pane().viewers.is_empty());
    assert_eq!(server.pane().input_holder, None, "a viewer takes no lease");
    viewer.send(b"echo VIEWER_$((6*7))\r");
    std::thread::sleep(SETTLE);
    assert!(
        !server.screen().contains("VIEWER_42"),
        "a viewer's typing reached the pane: the server must refuse a VIEWER \
         subscription's input"
    );

    let mut taker = common::PtyAttach::start(&server.socket, &["--take", SESSION], (110, 30), &[]);
    wait_until("the taker holds the lease", || {
        server.pane().input_holder.is_some()
    });
    taker.send(b"echo TAKER_$((6*7))\r");
    wait_until("the taker's line runs", || {
        server.screen().contains("TAKER_42")
    });
    assert!(
        !server.screen().contains("VIEWER_42"),
        "the viewer's refused line must never surface later"
    );
    drop((viewer, taker));
}
