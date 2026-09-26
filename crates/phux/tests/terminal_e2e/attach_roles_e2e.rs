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

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::ResourceInfo;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

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
        let out = Command::new(PHUX)
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
struct AttachedClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    _config: tempfile::TempDir,
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AttachedClient {
    fn start(server: &ServerGuard, role_flag: &str) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attach PTY");
        let config = tempfile::tempdir().expect("isolated config dir");
        let mut command = CommandBuilder::new(PHUX);
        command.args([
            "attach",
            role_flag,
            "--socket",
            server.socket.to_str().expect("UTF-8 socket"),
            SESSION,
        ]);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("XDG_CONFIG_HOME", config.path());
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);
        // Drain the paint stream so a full PTY buffer never stalls the TUI.
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(read) = reader.read(&mut bytes) {
                if read == 0 {
                    break;
                }
            }
        });
        let writer = pair.master.take_writer().expect("PTY writer");
        Self {
            child,
            writer,
            _config: config,
        }
    }

    fn type_line(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r").as_bytes())
            .expect("type into the attached TUI");
        self.writer.flush().expect("flush typed line");
    }
}

#[test]
#[ignore = "spawns a real phux server and attached PTY clients; run via `just e2e`."]
fn attach_take_seizes_and_attach_viewer_cannot_type() {
    let server = ServerGuard::start();

    let mut viewer = AttachedClient::start(&server, "--viewer");
    wait_until("the viewer is listed", || !server.pane().viewers.is_empty());
    assert_eq!(server.pane().input_holder, None, "a viewer takes no lease");
    viewer.type_line("echo VIEWER_$((6*7))");
    std::thread::sleep(SETTLE);
    assert!(
        !server.screen().contains("VIEWER_42"),
        "a viewer's typing reached the pane: the server must refuse a VIEWER \
         subscription's input"
    );

    let mut taker = AttachedClient::start(&server, "--take");
    wait_until("the taker holds the lease", || {
        server.pane().input_holder.is_some()
    });
    taker.type_line("echo TAKER_$((6*7))");
    wait_until("the taker's line runs", || {
        server.screen().contains("TAKER_42")
    });
    assert!(
        !server.screen().contains("VIEWER_42"),
        "the viewer's refused line must never surface later"
    );
    drop((viewer, taker));
}
