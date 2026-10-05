//! A QUIC attach restoring a persisted layout that names a dead pane.
//!
//! The persisted `phux.tui.layout/v1/<session>` tree outlives the panes it
//! names: a pane can exit, or be killed from the CLI, while no TUI is
//! attached to fold it out. Over QUIC multistream every Terminal frame rides
//! that Terminal's own stream, so a bootstrap reflow that sized the dead
//! leaf before any stream was bound ended the attach with "Terminal frame
//! requires a live QUIC binding". This drives the real binary: a real
//! server with a loopback QUIC listener, a two-pane layout whose second leaf
//! is already gone, and `phux attach --quic` on a PTY.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::layout::{LayoutNode, SplitDir, Workspace};
use phux_client::layout_ops::{LayoutOps, LayoutOpsError, layout_key};
use phux_protocol::ids::{GroupId, ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, Scope};

const SESSION: &str = "work";
const POLL: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(20);

/// The PTY the QUIC-attached client runs in.
const ATTACH_PTY: (u16, u16) = (100, 24);

/// The message the attach died with when the reflow reached the dead leaf.
const UNBOUND_TERMINAL: &str = "requires a live QUIC binding";

/// A real server on a private UDS plus a loopback QUIC listener, with every
/// directory it or a client reads or writes inside one tempdir.
struct QuicServer {
    inner: common::ServerGuard,
    dir: tempfile::TempDir,
    quic: std::net::SocketAddr,
}

impl QuicServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("server tempdir");
        for name in ["home", "config", "state", "runtime"] {
            std::fs::create_dir(dir.path().join(name)).expect("create isolated server directory");
        }
        let inner = common::ServerGuard::builder("quic-restore")
            .env("SHELL", "/bin/sh")
            .envs(isolated_env(dir.path()))
            .start_with(|cmd| {
                cmd.args(["--quic", listeners::LOOPBACK_ANY_PORT]);
            });
        let quic =
            listeners::bound_listener_addr(&inner.socket, listeners::RemoteListenerTransport::Quic);
        Self { inner, dir, quic }
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        let (verb, rest) = args.split_first().expect("verb");
        common::phux_cmd(crate::runner::phux_bin())
            .arg(verb)
            .arg("--socket")
            .arg(&self.inner.socket)
            .args(rest)
            .envs(isolated_env(self.dir.path()))
            .stdin(Stdio::null())
            .output()
            .expect("run phux command")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "phux {args:?} failed: stderr={}",
            String::from_utf8_lossy(&output.stderr),
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|err| panic!("invalid JSON for {args:?}: {err}"))
    }

    fn seed_pane(&self) -> ResourceId {
        let snapshot = self.json(&["snapshot", "--json", SESSION]);
        local_pane(&snapshot["pane"])
    }

    fn spawn_pane(&self) -> ResourceId {
        local_pane(&self.json(&["spawn", "--json"])["terminal_id"])
    }

    /// `(cols, rows)` of the pane actor's own grid, or `None` once the pane
    /// is gone.
    fn pane_size(&self, pane: &ResourceId) -> Option<(u64, u64)> {
        let selector = format!("@{}", pane.local_id().expect("local pane"));
        let output = self.command(&["snapshot", "--json", &selector]);
        if !output.status.success() {
            return None;
        }
        let snapshot: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("snapshot --json is JSON");
        Some((
            snapshot["cols"].as_u64().expect("snapshot cols"),
            snapshot["rows"].as_u64().expect("snapshot rows"),
        ))
    }

    /// Kill `pane` from the CLI and wait until the server no longer has it.
    fn kill_pane(&self, pane: &ResourceId) {
        let selector = format!("@{}", pane.local_id().expect("local pane"));
        let output = self.command(&["kill", "--yes", &selector]);
        assert!(
            output.status.success(),
            "kill {selector}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let deadline = Instant::now() + DEADLINE;
        while self.pane_size(pane).is_some() {
            assert!(Instant::now() < deadline, "{selector} outlived its kill");
            std::thread::sleep(POLL);
        }
    }

    /// Persist `workspace` as the session's layout, the way an attached TUI
    /// would have before the pane died. The ordered GET is a barrier proving
    /// the fire-and-forget SET landed before the attach starts.
    fn write_layout(&self, workspace: &Workspace) {
        let runtime = runtime();
        runtime.block_on(async {
            let mut conn = Connection::connect(&self.inner.socket)
                .await
                .expect("connect metadata writer");
            conn.send(&FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(SessionId::new(1)),
                value: workspace.encode_cbor().expect("encode layout"),
            })
            .await
            .expect("write layout");
            conn.send(&FrameKind::GetMetadata {
                request_id: 2,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(SessionId::new(1)),
            })
            .await
            .expect("request layout barrier");
            loop {
                if let FrameKind::MetadataValue {
                    request_id: 2,
                    value: Some(_),
                } = conn.recv().await.expect("layout barrier reply")
                {
                    break;
                }
            }
        });
    }

    fn read_layout(&self) -> Result<Workspace, LayoutOpsError> {
        runtime().block_on(async {
            let mut conn = Connection::connect(&self.inner.socket).await?;
            let result = LayoutOps::new(&mut conn, SessionId::new(1), 1).read().await;
            drop(conn);
            result
        })
    }

    /// `phux attach --quic` to this server's listener on a PTY. Config and
    /// state both resolve inside the tempdir, so the attach never reads the
    /// real config or dials the running server.
    fn attach_over_quic(&self) -> common::PtyAttach {
        let addr = self.quic.to_string();
        let dir = self.dir.path().to_owned();
        common::PtyAttach::start_dialing(&["--quic", &addr, SESSION], ATTACH_PTY, |_, command| {
            for (key, value) in isolated_env(&dir) {
                if key != "XDG_CONFIG_HOME" {
                    command.env(key, value);
                }
            }
        })
    }
}

/// `HOME` and the XDG roots inside `dir`. The attach keeps the empty config
/// dir [`common::PtyAttach`] gives it, so the embedded defaults apply.
fn isolated_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("HOME", dir.join("home")),
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("runtime")),
    ]
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn local_pane(id: &serde_json::Value) -> ResourceId {
    ResourceId::local(u32::try_from(id.as_u64().expect("pane id")).expect("pane id fits u32"))
}

/// The content rect a default-config attach leaves for panes on
/// [`ATTACH_PTY`]: the sidebar's columns and the status bar and pane-title
/// rows come off the viewport (as `resize_e2e` pins for a UDS attach).
fn full_content_rect() -> (u64, u64) {
    let (cols, rows) = ATTACH_PTY;
    let sidebar = phux_config::SidebarCfg::default();
    let sidebar_width = if sidebar.width == 0 {
        (cols / 4).clamp(28, 40)
    } else {
        sidebar.width
    };
    (
        u64::from(cols) - u64::from(sidebar_width),
        u64::from(rows) - 2,
    )
}

#[test]
#[ignore = "spawns a real server with a QUIC listener and an attached PTY client; run in the e2e lane"]
fn quic_attach_survives_a_layout_naming_a_dead_pane() {
    let server = QuicServer::start();
    let live = server.seed_pane();
    let dead = server.spawn_pane();
    server.kill_pane(&dead);

    // The layout is written after the kill, so nothing has folded the dead
    // leaf out: this is the tree a TUI left behind before the pane died.
    let mut layout = Workspace::single(live.clone());
    layout.windows[0].state.tree = Some(LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(live.clone())),
        right: Box::new(LayoutNode::Leaf(dead.clone())),
    });
    server.write_layout(&layout);

    let mut client = server.attach_over_quic();

    // With the dead leaf folded out, the live pane owns the whole content
    // rect; half of it would mean the dead leaf was still being laid out.
    let want = full_content_rect();
    let deadline = Instant::now() + DEADLINE;
    let mut seen = server.pane_size(&live);
    while seen != Some(want) {
        assert_attach_up(&mut client);
        assert!(
            Instant::now() < deadline,
            "the live pane never reached the full content rect {want:?} (last {seen:?}):\n{}",
            client.painted()
        );
        std::thread::sleep(POLL);
        seen = server.pane_size(&live);
    }

    // The client re-persists the pruned tree: one leaf, the live pane.
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert_attach_up(&mut client);
        let tree = server
            .read_layout()
            .expect("persisted layout")
            .windows
            .first()
            .and_then(|window| window.state.tree.clone());
        if tree == Some(LayoutNode::Leaf(live.clone())) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the dead leaf {dead:?} was never pruned from the persisted layout: {tree:?}"
        );
        std::thread::sleep(POLL);
    }
    assert_attach_up(&mut client);
}

/// The attach is still running and has not reported a Terminal frame with
/// no QUIC stream.
fn assert_attach_up(client: &mut common::PtyAttach) {
    let running = client.is_running();
    let painted = client.painted();
    assert!(
        running && !painted.contains(UNBOUND_TERMINAL),
        "the QUIC attach did not survive restoring the layout (running: {running}):\n{painted}"
    );
}
