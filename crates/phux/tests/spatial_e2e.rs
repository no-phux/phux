//! Real-server end-to-end coverage for the existing-pane spatial CLI.
//!
//! A real `phux server` runs on a private UDS and a real TUI client remains
//! attached through a pseudo-terminal while separate CLI subprocesses insert,
//! move, and swap panes. Persisted trees are decoded back through `LayoutOps`,
//! and marker commands typed through the attached client prove metadata
//! reconciliation preserves that client's local focus.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::layout::{LayoutNode, SplitDir, Workspace};
use phux_client::layout_ops::{LayoutOps, LayoutOpsError, layout_key};
use phux_protocol::ids::{GroupId, SessionId, TerminalId};
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
static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ServerGuard {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerGuard {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("server tempdir");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("spatial-{}-{n}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self {
            child,
            socket,
            _dir: dir,
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

    fn seed_pane(&self) -> TerminalId {
        let snapshot = self.json(&["snapshot", "--json", SESSION]);
        TerminalId::local(
            u32::try_from(snapshot["pane"].as_u64().expect("snapshot pane id"))
                .expect("pane id fits u32"),
        )
    }

    fn spawn_pane(&self) -> TerminalId {
        let spawned = self.json(&["spawn", "--json"]);
        TerminalId::local(
            u32::try_from(spawned["terminal_id"].as_u64().expect("spawn terminal id"))
                .expect("terminal id fits u32"),
        )
    }

    fn seed_layout(&self, pane: &TerminalId) {
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async {
            let mut conn = Connection::connect(&self.socket).await?;
            LayoutOps::new(&mut conn, SessionId::new(1), 1).read().await
        })
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

    fn pane_contains(&self, pane: &TerminalId, marker: &str) -> bool {
        let selector = format!("@{}", pane.local_id().expect("local pane"));
        let snapshot = self.json(&["snapshot", "--json", &selector]);
        snapshot["lines"]
            .as_array()
            .expect("snapshot lines")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|line| line.contains(marker))
    }

    fn wait_for_marker(&self, pane: &TerminalId, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pane_contains(pane, marker) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("marker {marker:?} did not reach {pane:?}");
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
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 24,
                cols: 100,
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
        self.writer.write_all(b"\x01o").expect("send C-a o");
        self.writer.flush().expect("flush focus chord");
        std::thread::sleep(Duration::from_millis(300));
    }

    fn type_marker(&mut self, marker: &str) {
        self.writer
            .write_all(format!("echo {marker}\r").as_bytes())
            .expect("type marker through attached client");
        self.writer.flush().expect("flush marker");
    }
}

fn leaf(id: &TerminalId) -> LayoutNode {
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

    let second = server.spawn_pane();
    let third = server.spawn_pane();

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
    std::thread::sleep(Duration::from_millis(300));
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
    std::thread::sleep(Duration::from_millis(300));
    attached.type_marker("FOCUS_AFTER_HORIZONTAL_INSERT");
    server.wait_for_marker(&second, "FOCUS_AFTER_HORIZONTAL_INSERT");
    assert!(!server.pane_contains(&third, "FOCUS_AFTER_HORIZONTAL_INSERT"));

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
    std::thread::sleep(Duration::from_millis(300));
    attached.type_marker("FOCUS_AFTER_MOVE");
    server.wait_for_marker(&second, "FOCUS_AFTER_MOVE");

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
    std::thread::sleep(Duration::from_millis(300));
    attached.type_marker("FOCUS_AFTER_SWAP");
    server.wait_for_marker(&second, "FOCUS_AFTER_SWAP");
}
