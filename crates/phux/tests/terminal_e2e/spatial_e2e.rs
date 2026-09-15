//! Real-server end-to-end coverage for the existing-pane spatial CLI.
//!
//! A real `phux server` runs on a private UDS and a real TUI client remains
//! attached through a pseudo-terminal while separate CLI subprocesses insert,
//! move, and swap panes. Persisted trees are decoded back through `LayoutOps`,
//! and marker commands typed through the attached client prove metadata
//! reconciliation preserves that client's local focus.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::layout::{LayoutNode, SplitDir, Workspace};
use phux_client::layout_ops::{LayoutOps, LayoutOpsError, layout_key};
use phux_protocol::ids::{GroupId, ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Idle lifetime for this file's harness server, as a backstop UNDER the
/// `Drop` kill (ADR-0063). The guard is still the primary cleanup; it cannot
/// run if the test process is `SIGKILL`ed or the runner is reaped mid-job, and
/// what leaks then is a daemon holding a live PTY on a socket nobody will
/// ever look at again. Ten minutes is far longer than any gap between this
/// file's client connections, so it can only fire after the harness is gone.
const SERVER_IDLE_LIMIT_SECS: &str = "600";

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
const SESSION: &str = "work";
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// Unattached no-TTY grid from `GET_SCREEN`. A tiled pane leaving this size
/// (or any previously observed size) is how this file knows the attached
/// client reconciled a layout broadcast — `RESIZE_TERMINAL` is the side
/// effect of that reconcile, not of the CLI `SET_METADATA` itself.
const NO_TTY_DEFAULT: (u64, u64) = (80, 24);
static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

impl ServerGuard {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("server tempdir");
        for name in ["home", "config", "state", "runtime"] {
            std::fs::create_dir(dir.path().join(name)).expect("create isolated server directory");
        }
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("spatial-{}-{n}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            // Panes run the server's `$SHELL`. Inherited, that is the CI
            // devshell's minimal bash sourcing the runner's `~/.bashrc`, whose
            // startup noise buries the typed markers; pin the `/bin/sh` the
            // marker barriers assume.
            .env("SHELL", "/bin/sh")
            .env("HOME", dir.path().join("home"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", dir.path().join("runtime"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
            socket,
            dir,
        };
        let deadline = Instant::now() + SOCKET_DEADLINE;
        while Instant::now() < deadline {
            if guard.socket.exists() {
                return guard;
            }
            std::thread::sleep(POLL);
        }
        panic!("server did not bind {}", guard.socket.display());
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        let (verb, rest) = args.split_first().expect("verb");
        Command::new(PHUX)
            .arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.dir.path().join("runtime"))
            .stdin(Stdio::null())
            .output()
            .expect("run phux command")
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "phux {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let stdout = self.success(args);
        serde_json::from_str(&stdout)
            .unwrap_or_else(|err| panic!("invalid JSON for {args:?}: {err}: {stdout}"))
    }

    fn seed_pane(&self) -> ResourceId {
        let snapshot = self.json(&["snapshot", "--json", SESSION]);
        ResourceId::local(
            u32::try_from(snapshot["pane"].as_u64().expect("snapshot pane id"))
                .expect("pane id fits u32"),
        )
    }

    fn spawn_pane(&self) -> ResourceId {
        let spawned = self.json(&["spawn", "--json"]);
        ResourceId::local(
            u32::try_from(spawned["terminal_id"].as_u64().expect("spawn terminal id"))
                .expect("terminal id fits u32"),
        )
    }

    fn seed_layout(&self, pane: &ResourceId) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async {
            let mut conn = Connection::connect(&self.socket)
                .await
                .expect("connect metadata client");
            let workspace = Workspace::single(pane.clone());
            conn.send(&FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(SessionId::new(1)),
                value: workspace.encode_cbor().expect("encode seed layout"),
            })
            .await
            .expect("seed layout metadata");
            // The ordered GET is a barrier proving the fire-and-forget SET was
            // consumed before the real TUI attaches.
            conn.send(&FrameKind::GetMetadata {
                request_id: 2,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(SessionId::new(1)),
            })
            .await
            .expect("request seeded layout");
            loop {
                if let FrameKind::MetadataValue {
                    request_id: 2,
                    value: Some(_),
                } = conn.recv().await.expect("seed layout reply")
                {
                    break;
                }
            }
        });
    }

    fn read_layout(&self) -> Result<Workspace, LayoutOpsError> {
        self.read_layout_for(SESSION)
    }

    fn read_layout_for(&self, session_name: &str) -> Result<Workspace, LayoutOpsError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async {
            let mut conn = Connection::connect(&self.socket).await?;
            let snapshot = phux_client::state::get_state_on(&mut conn)
                .await?
                .into_parts()
                .0;
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.name == session_name)
                .unwrap_or_else(|| panic!("session {session_name:?} missing from snapshot"));
            let result = LayoutOps::new(&mut conn, session.id, 1).read().await;
            drop(conn);
            result
        })
    }

    fn write_layout_bytes(&self, session_name: &str, value: Vec<u8>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async {
            let mut conn = Connection::connect(&self.socket)
                .await
                .expect("connect metadata writer");
            let snapshot = phux_client::state::get_state_on(&mut conn)
                .await
                .expect("read state")
                .into_parts()
                .0;
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.name == session_name)
                .expect("session to corrupt");
            conn.send(&FrameKind::SetMetadata {
                request_id: 30,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(session.id),
                value,
            })
            .await
            .expect("write layout bytes");
            conn.send(&FrameKind::GetMetadata {
                request_id: 31,
                scope: Scope::Group(GroupId::new(1)),
                key: layout_key(session.id),
            })
            .await
            .expect("request layout write barrier");
            loop {
                if matches!(
                    conn.recv().await.expect("layout write barrier reply"),
                    FrameKind::MetadataValue {
                        request_id: 31,
                        value: Some(_)
                    }
                ) {
                    break;
                }
            }
        });
    }

    fn wait_for_layout(&self) -> Workspace {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.read_layout() {
                Ok(layout) => return layout,
                Err(LayoutOpsError::MissingLayout) => std::thread::sleep(POLL),
                Err(err) => panic!("read persisted layout: {err}"),
            }
        }
        panic!("attached client did not seed layout metadata");
    }

    fn pane_snapshot(&self, pane: &ResourceId) -> serde_json::Value {
        let selector = format!("@{}", pane.local_id().expect("local pane"));
        self.json(&["snapshot", "--json", &selector])
    }

    fn pane_size(&self, pane: &ResourceId) -> (u64, u64) {
        let snapshot = self.pane_snapshot(pane);
        (
            snapshot["cols"].as_u64().expect("snapshot cols"),
            snapshot["rows"].as_u64().expect("snapshot rows"),
        )
    }

    fn wait_for_pane_size(&self, pane: &ResourceId, expected: (u64, u64)) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pane_size(pane) == expected {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "pane {pane:?} did not reach {expected:?}; last size {:?}",
            self.pane_size(pane)
        );
    }

    fn wait_for_attached(&self, session_name: &str, expected: bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let output = self.command(&["ls", "--json"]);
            let Ok(document) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
                std::thread::sleep(POLL);
                continue;
            };
            let attached = document["sessions"]
                .as_array()
                .expect("sessions array")
                .iter()
                .find(|session| session["name"] == session_name)
                .and_then(|session| session["attached"].as_bool());
            if attached == Some(expected) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("session {session_name:?} did not reach attached={expected}");
    }

    /// Wait until `pane`'s live grid differs from `before`.
    ///
    /// The spatial CLI persists topology and returns; the attached client
    /// adopts the tree on `METADATA_CHANGED` and only then resizes. A fixed
    /// 300ms sleep after the CLI verb is a load-dependent bet (phux-5wxp.1):
    /// under contention the broadcast loses to the next typed marker, so
    /// focus probes land on the pre-reconcile tree (or the PTY write fails
    /// because the client tore down mid-interleave). Snapshot cols/rows are
    /// the pane actor's own grid, so a change is proof the client consumed
    /// the update — the same "reply, not send" distinction the family uses.
    fn wait_for_applied_grid(&self, pane: &ResourceId, before: (u64, u64)) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let seen = self.pane_size(pane);
            if seen != before {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("attached client did not apply a new grid for {pane:?} (still {before:?})");
    }

    fn pane_lines(&self, pane: &ResourceId) -> Vec<String> {
        self.pane_snapshot(pane)["lines"]
            .as_array()
            .expect("snapshot lines")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .collect()
    }

    fn pane_has_output(&self, pane: &ResourceId) -> bool {
        self.pane_lines(pane)
            .iter()
            .any(|line| !line.trim().is_empty())
    }

    /// Chrome resize proves the client attached and reflowed; it does not
    /// prove the seed pane's `/bin/sh` has been scheduled (phux-5wxp shape
    /// 5). Typed markers ride INPUT to that shell, so a blank grid here is
    /// an ambient miss, not a focus regression.
    fn wait_for_shell_output(&self, pane: &ResourceId) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pane_has_output(pane) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("pane {pane:?} never painted shell output");
    }

    fn pane_contains(&self, pane: &ResourceId, marker: &str) -> bool {
        self.pane_lines(pane)
            .iter()
            .any(|line| line.contains(marker))
    }

    fn wait_for_marker(&self, pane: &ResourceId, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pane_contains(pane, marker) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!(
            "marker {marker:?} did not reach {pane:?} (grid {:?}, lines {:?})",
            self.pane_size(pane),
            self.pane_lines(pane)
        );
    }
}

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
    fn start(server: &ServerGuard) -> Self {
        Self::start_named_at_size(server, SESSION, 100, 24)
    }

    fn start_named_at_size(server: &ServerGuard, session: &str, cols: u16, rows: u16) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open attach PTY");
        let config = tempfile::tempdir().expect("isolated config dir");
        let mut command = CommandBuilder::new(PHUX);
        command.args([
            "attach",
            "--socket",
            server.socket.to_str().expect("UTF-8 socket"),
            session,
        ]);
        command.env("SHELL", "/bin/sh");
        command.env("TERM", "xterm-256color");
        command.env("RUST_LOG", "off");
        command.env("HOME", server.dir.path().join("home"));
        command.env("XDG_CONFIG_HOME", config.path());
        command.env("XDG_STATE_HOME", server.dir.path().join("state"));
        command.env("XDG_RUNTIME_DIR", server.dir.path().join("runtime"));
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);

        // Drain paint output continuously so the PTY cannot backpressure the
        // real client while the test drives metadata and input concurrently.
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(read) = reader.read(&mut bytes) {
                if read == 0 {
                    break;
                }
            }
        });
        let writer = pair.master.take_writer().expect("take PTY writer");
        Self {
            child,
            writer,
            _config: config,
        }
    }

    fn next_pane(&mut self) {
        // Applied in the same input turn as a following `type_marker` write.
        // A settle sleep here is a load-dependent bet (phux-5wxp.1).
        self.writer.write_all(b"\x01o").expect("send C-a o");
        self.writer.flush().expect("flush focus chord");
    }

    fn type_marker(&mut self, marker: &str) {
        self.writer
            .write_all(format!("echo {marker}\r").as_bytes())
            .expect("type marker through attached client");
        self.writer.flush().expect("flush marker");
    }

    fn detach(&mut self) {
        self.writer.write_all(b"\x01d").expect("send C-a d");
        self.writer.flush().expect("flush detach chord");
    }
}

fn leaf(id: &ResourceId) -> LayoutNode {
    LayoutNode::Leaf(id.clone())
}

fn split(dir: SplitDir, left: LayoutNode, right: LayoutNode) -> LayoutNode {
    LayoutNode::Split {
        dir,
        ratio: 0.5,
        left: Box::new(left),
        right: Box::new(right),
    }
}

fn assert_tree(server: &ServerGuard, expected: LayoutNode) {
    let layout = server.read_layout().expect("persisted layout");
    assert_eq!(layout.windows.len(), 1);
    assert_eq!(layout.windows[0].state.tree, Some(expected));
}

#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn spatial_cli_persists_topology_and_preserves_attached_focus() {
    let server = ServerGuard::start();
    let seed = server.seed_pane();
    server.seed_layout(&seed);
    let mut attached = AttachedClient::start(&server);
    let initial = server.wait_for_layout();
    assert_eq!(initial.windows[0].state.tree, Some(leaf(&seed)));
    // Metadata was seeded before attach, so `wait_for_layout` can return
    // before this client has subscribed. Chrome resize plus a painted
    // prompt is the attach barrier (client ready *and* the seed shell).
    server.wait_for_applied_grid(&seed, NO_TTY_DEFAULT);
    server.wait_for_shell_output(&seed);
    let seed_attached = server.pane_size(&seed);

    let second = server.spawn_pane();
    let third = server.spawn_pane();
    let third_unplaced = server.pane_size(&third);

    // User-facing `vertical` means a vertical divider and side-by-side panes;
    // the persisted child axis is therefore internal Horizontal.
    let inserted = server.json(&[
        "insert-pane",
        &format!("@{}", seed.local_id().expect("seed id")),
        &format!("@{}", second.local_id().expect("second id")),
        "--split",
        "vertical",
        "--json",
    ]);
    assert_eq!(inserted["direction"], "vertical");
    assert_tree(
        &server,
        split(SplitDir::Horizontal, leaf(&seed), leaf(&second)),
    );
    server.wait_for_applied_grid(&seed, seed_attached);
    attached.type_marker("FOCUS_AFTER_VERTICAL_INSERT");
    server.wait_for_marker(&seed, "FOCUS_AFTER_VERTICAL_INSERT");
    assert!(!server.pane_contains(&second, "FOCUS_AFTER_VERTICAL_INSERT"));

    // Move local focus to pane two. The next metadata writer focuses pane
    // three in its serialized envelope, but ADR-0049 reconciliation must keep
    // this attached client on pane two.
    attached.next_pane();
    attached.type_marker("FOCUS_ON_SECOND");
    server.wait_for_marker(&second, "FOCUS_ON_SECOND");

    let inserted = server.json(&[
        "insert-pane",
        &format!("@{}", second.local_id().expect("second id")),
        &format!("@{}", third.local_id().expect("third id")),
        "--split",
        "horizontal",
        "--json",
    ]);
    assert_eq!(inserted["direction"], "horizontal");
    assert_tree(
        &server,
        split(
            SplitDir::Horizontal,
            leaf(&seed),
            split(SplitDir::Vertical, leaf(&second), leaf(&third)),
        ),
    );
    server.wait_for_applied_grid(&third, third_unplaced);
    attached.type_marker("FOCUS_AFTER_HORIZONTAL_INSERT");
    server.wait_for_marker(&second, "FOCUS_AFTER_HORIZONTAL_INSERT");
    assert!(!server.pane_contains(&third, "FOCUS_AFTER_HORIZONTAL_INSERT"));

    let seed_before_move = server.pane_size(&seed);
    server.success(&[
        "move-pane",
        &format!("@{}", seed.local_id().expect("seed id")),
        &format!("@{}", third.local_id().expect("third id")),
        "--split",
        "vertical",
    ]);
    assert_tree(
        &server,
        split(
            SplitDir::Vertical,
            leaf(&second),
            split(SplitDir::Horizontal, leaf(&third), leaf(&seed)),
        ),
    );
    server.wait_for_applied_grid(&seed, seed_before_move);
    attached.type_marker("FOCUS_AFTER_MOVE");
    server.wait_for_marker(&second, "FOCUS_AFTER_MOVE");

    let second_before_swap = server.pane_size(&second);
    server.success(&[
        "swap-pane",
        &format!("@{}", second.local_id().expect("second id")),
        &format!("@{}", third.local_id().expect("third id")),
    ]);
    assert_tree(
        &server,
        split(
            SplitDir::Vertical,
            leaf(&third),
            split(SplitDir::Horizontal, leaf(&second), leaf(&seed)),
        ),
    );
    server.wait_for_applied_grid(&second, second_before_swap);
    attached.type_marker("FOCUS_AFTER_SWAP");
    server.wait_for_marker(&second, "FOCUS_AFTER_SWAP");
}

fn json_created_pane(server: &ServerGuard, session: &str) -> ResourceId {
    let created = server.json(&["new", "--json", "-s", session]);
    ResourceId::local(
        u32::try_from(created["terminal_id"].as_u64().expect("created pane id"))
            .expect("pane id fits u32"),
    )
}

fn run_in_pane(server: &ServerGuard, pane: &ResourceId, command: &str) -> serde_json::Value {
    server.json(&[
        "run",
        "--json",
        &format!("@{}", pane.local_id().expect("local pane")),
        command,
    ])
}

#[test]
#[ignore = "spawns a real server and PTY-backed shells; run in the e2e lane"]
fn json_created_sessions_are_immediately_layout_ready_for_cross_session_move() {
    let server = ServerGuard::start();
    let source = json_created_pane(&server, "headless-source");
    let destination = json_created_pane(&server, "headless-destination");

    assert_eq!(
        server
            .read_layout_for("headless-source")
            .expect("source layout seeded"),
        Workspace::single(source.clone())
    );
    assert_eq!(
        server
            .read_layout_for("headless-destination")
            .expect("destination layout seeded"),
        Workspace::single(destination.clone())
    );

    let before = run_in_pane(&server, &source, "printf 'ALF7_BEFORE:%s\\n' \"$$\"");
    let before_output = before["output"].as_str().expect("before output");
    let shell_pid = before_output
        .trim()
        .strip_prefix("ALF7_BEFORE:")
        .expect("before marker carries shell pid")
        .to_owned();

    let moved = server.json(&[
        "move-pane",
        &format!("@{}", source.local_id().expect("source id")),
        &format!("@{}", destination.local_id().expect("destination id")),
        "--split",
        "vertical",
        "--json",
    ]);
    assert_eq!(moved["cross_session"], true);
    assert!(moved["source_session_id"].as_u64().is_some());
    let destination_layout = server
        .read_layout_for("headless-destination")
        .expect("moved destination layout");
    assert_eq!(
        phux_client::layout::leaves(
            destination_layout.windows[0]
                .state
                .tree
                .as_ref()
                .expect("destination tree")
        ),
        vec![destination, source.clone()]
    );

    let after = run_in_pane(&server, &source, "printf 'ALF7_AFTER:%s\\n' \"$$\"");
    assert_eq!(
        after["output"].as_str().expect("after output").trim(),
        format!("ALF7_AFTER:{shell_pid}"),
        "the same shell process must survive the ownership/layout move"
    );
    let screen = server.pane_snapshot(&source);
    let retained = screen["lines"]
        .as_array()
        .expect("screen lines")
        .iter()
        .chain(screen["scrollback"].as_array().into_iter().flatten())
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        retained.contains("ALF7_BEFORE:"),
        "pre-move output was lost: {retained}"
    );
    assert!(
        retained.contains("ALF7_AFTER:"),
        "post-move output missing: {retained}"
    );
}

#[test]
#[ignore = "spawns a real server and PTY client; run in the e2e lane"]
fn last_tiny_view_detach_restores_headless_geometry_and_keeps_the_shell() {
    let server = ServerGuard::start();
    let pane = json_created_pane(&server, "tiny-headless");
    let mut attached = AttachedClient::start_named_at_size(&server, "tiny-headless", 1, 1);
    server.wait_for_attached("tiny-headless", true);
    server.wait_for_pane_size(&pane, (1, 1));

    attached.detach();
    server.wait_for_attached("tiny-headless", false);
    server.wait_for_pane_size(&pane, NO_TTY_DEFAULT);
    let result = run_in_pane(&server, &pane, "printf ALF7_STILL_ALIVE");
    assert_eq!(result["output"], "ALF7_STILL_ALIVE");
}

#[test]
#[ignore = "spawns a real server and PTY-backed shells; run in the e2e lane"]
fn malformed_destination_layout_keeps_the_json_error_contract() {
    let server = ServerGuard::start();
    let source = json_created_pane(&server, "bad-layout-source");
    let destination = json_created_pane(&server, "bad-layout-destination");
    server.write_layout_bytes("bad-layout-destination", b"not-cbor".to_vec());

    let output = server.command(&[
        "move-pane",
        &format!("@{}", source.local_id().expect("source id")),
        &format!("@{}", destination.local_id().expect("destination id")),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "JSON failure must keep stdout empty"
    );
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|err| panic!("structured stderr: {err}: {:?}", output.stderr));
    assert_eq!(error["schema_version"], 1);
    assert_eq!(error["error"]["code"], "destination_layout_failed");
    assert_eq!(error["exit_code"], 1);
    assert!(
        error["remedy"]
            .as_str()
            .is_some_and(|remedy| !remedy.is_empty())
    );
}
