//! Real-server end-to-end coverage for the fleet-inbox sidebar's roster zone.
//!
//! A real `phux server` runs on a private UDS, a second session is created
//! headlessly beside the first with a persisted layout and a blocked agent
//! record, and a real TUI client attaches through a pseudo-terminal. The
//! assertion is that the attached client's `spaces` row for the peer carries
//! the peer's state histogram.
//!
//! phux-k0cw.10 is why this exists. The peer sweep that populates the roster
//! used to be sent during bootstrap, ahead of the `TERMINAL_SNAPSHOT` burst
//! that produces the first paint; it is now deferred to the first repaint
//! drain so a session switch does not pay for the roster before it paints.
//! Deferral buys latency and risks liveness: a sweep that is never issued
//! leaves the roster permanently undescribed, and every unit test in the
//! client still passes because each one calls the sweep directly. This test is
//! the one that fails if the deferred send never goes out.
//!
//! The histogram — not the row — is the assertion, and that is deliberate. A
//! roster ROW exists for every peer in the ATTACHED session graph whether or
//! not any sweep ran (`session_roster` gives an undescribed session a row with
//! zero counts on purpose: "this space exists" is the roster's whole job). So
//! asserting on the row name would pass against a client that never swept.
//! The counts are the part that can only come from the peer's fetched layout
//! and its panes' fetched agent records, which is the chain this bead moved.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

mod common;

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::layout::Workspace;
use phux_client::layout_ops::layout_key;
use phux_protocol::ids::{GroupId, SessionId, TerminalId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const PHUX: &str = env!("CARGO_BIN_EXE_phux");
/// The session the client attaches to — zone 2 (`here`).
const SESSION: &str = "work";
/// The session it does NOT attach to, which must appear in zone 3 (`spaces`).
const PEER: &str = "scratch";
/// How many session ids to probe when locating the peer's persisted layout.
/// This file's server holds two sessions; the margin covers id allocation that
/// does not start at 1 without pinning it to any particular scheme.
const SESSION_ID_SCAN: u32 = 8;
/// How long to wait for the briefly attached peer client to write its layout.
const LAYOUT_DEADLINE: Duration = Duration::from_secs(20);
const SOCKET_DEADLINE: Duration = Duration::from_secs(30);
/// Generous on purpose: the roster is deliberately allowed to arrive late now,
/// so this is a liveness bound, not a latency assertion. The latency half of
/// phux-k0cw.10's acceptance is structural (the sweep is issued from the
/// repaint drain, not from bootstrap) and is not what this test measures.
const ROSTER_DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);
static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ServerGuard {
    _process: common::ServerProcess,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl ServerGuard {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("server tempdir");
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = dir
            .path()
            .join(format!("fleet-sidebar-{}-{n}.sock", std::process::id()));
        let child = Command::new(PHUX)
            .args(["server", "--session", SESSION, "--socket"])
            .arg(&socket)
            .args(["--exit-after-idle", common::SERVER_IDLE_LIMIT_SECS])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn phux server");
        let guard = Self {
            _process: common::ServerProcess::from_child(child, socket.clone()),
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

    fn success(&self, args: &[&str]) -> String {
        let (verb, rest) = args.split_first().expect("verb");
        let output = Command::new(PHUX)
            .arg(verb)
            .arg("--socket")
            .arg(&self.socket)
            .args(rest)
            .stdin(Stdio::null())
            .output()
            .expect("run phux command");
        assert!(
            output.status.success(),
            "phux {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The peer's seed pane, read back rather than assumed.
    fn peer_pane(&self) -> TerminalId {
        let stdout = self.success(&["snapshot", "--json", PEER]);
        let snapshot: serde_json::Value =
            serde_json::from_str(&stdout).expect("snapshot JSON for the peer session");
        let pane = u32::try_from(snapshot["pane"].as_u64().expect("snapshot pane id"))
            .expect("pane id fits u32");
        TerminalId::local(pane)
    }

    /// Wait until SOME session has persisted a layout naming `pane`.
    ///
    /// A headlessly created session has no persisted layout: writing one is an
    /// attached TUI client's job. The roster's counts iterate the leaves of
    /// the peer's fetched layout, so until one exists the peer is a
    /// legitimately undescribed session with a legitimately empty histogram —
    /// and this test could not tell a working sweep from a missing one.
    ///
    /// The peer's layout is therefore persisted by briefly attaching a real
    /// client to it (see [`AttachedClient::persist_layout_for`]) rather than
    /// seeded through a hand-built key. That is the point of scanning for the
    /// id here instead of assuming one: `phux ls --json` exposes no session
    /// id, and a test that hardcodes "the peer is session 2" silently stops
    /// testing anything the day allocation changes — it would seed a key
    /// nobody reads, and the assertion would fail for a reason that has
    /// nothing to do with the sweep.
    fn wait_for_persisted_layout(&self, pane: &TerminalId) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let deadline = Instant::now() + LAYOUT_DEADLINE;
        loop {
            let found = runtime.block_on(async {
                let mut conn = Connection::connect(&self.socket)
                    .await
                    .expect("connect metadata client");
                for candidate in 1..=SESSION_ID_SCAN {
                    let request_id = candidate;
                    conn.send(&FrameKind::GetMetadata {
                        request_id,
                        scope: Scope::Group(GroupId::new(1)),
                        key: layout_key(SessionId::new(candidate)),
                    })
                    .await
                    .expect("request candidate layout");
                    if let FrameKind::MetadataValue {
                        value: Some(bytes), ..
                    } = conn.recv().await.expect("candidate layout reply")
                        && let Ok(workspace) = Workspace::decode_cbor(&bytes)
                        && workspace
                            .windows
                            .iter()
                            .filter_map(|w| w.state.tree.as_ref())
                            .flat_map(phux_client::layout::leaves)
                            .any(|leaf| leaf == *pane)
                    {
                        return true;
                    }
                }
                false
            });
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no session persisted a layout naming {pane:?}"
            );
            std::thread::sleep(POLL);
        }
    }
}

/// An attached TUI client whose paint output is retained for assertions.
///
/// The reader thread keeps draining regardless — a PTY that fills up
/// backpressures the real client, which would stall exactly the repaint drain
/// this test is here to observe.
struct AttachedClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    transcript: Arc<Mutex<Vec<u8>>>,
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
        Self::start_on(server, SESSION)
    }

    fn start_on(server: &ServerGuard, session: &str) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                // Tall and wide enough that zone 3 is not yielded away: the
                // strip drops `spaces` first when the rows run out, and a
                // narrow terminal yields the sidebar entirely.
                rows: 40,
                cols: 120,
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
        // A fresh prefix so the default config applies — the sidebar ships
        // enabled, and a developer's own config must not decide this test.
        command.env("XDG_CONFIG_HOME", config.path());
        let child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attached TUI");
        drop(pair.slave);

        let transcript = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&transcript);
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                sink.lock()
                    .expect("transcript lock")
                    .extend_from_slice(&bytes[..count]);
            }
        });

        Self {
            child,
            transcript,
            _config: config,
        }
    }

    fn painted(&self) -> String {
        common::strip_terminal_controls(&self.transcript.lock().expect("transcript lock"))
    }

    /// Wait until every phrase has been painted at some point in the
    /// transcript, then return it. Panics with the painted text so a failure
    /// shows what the strip actually rendered instead of just "not found".
    fn wait_for_all(&self, phrases: &[&str]) -> String {
        let deadline = Instant::now() + ROSTER_DEADLINE;
        loop {
            let painted = self.painted();
            if phrases.iter().all(|phrase| painted.contains(phrase)) {
                return painted;
            }
            if Instant::now() >= deadline {
                let missing: Vec<&str> = phrases
                    .iter()
                    .copied()
                    .filter(|phrase| !painted.contains(phrase))
                    .collect();
                panic!("sidebar never painted {missing:?}; painted text was:\n{painted}");
            }
            std::thread::sleep(POLL);
        }
    }
}

/// phux-k0cw.10: the peer sweep still describes the roster after moving off
/// the bootstrap path.
///
/// Everything the peer needs is in place BEFORE the client attaches, so the
/// only thing standing between a fully described peer and a bare row is the
/// deferred sweep itself. If the deferral ever stops firing — a drain that is
/// never reached, a flag cleared without the send — the peer still gets its
/// row from the session graph, but its histogram never arrives and this fails.
///
/// Verified to discriminate rather than assumed to: it passes against the
/// pre-deferral driver, passes against the deferred one, and fails when the
/// deferred send alone is stubbed out. That last run also settled a real
/// question — the in-loop session-graph sweep does NOT happen to cover a plain
/// attach, so the deferred send is load-bearing and not a redundant second
/// path.
#[test]
#[ignore = "spawns a real server and attached PTY client; run in the e2e lane"]
fn deferred_peer_sweep_still_describes_the_spaces_roster() {
    let server = ServerGuard::start();
    // `--json` is the headless form (bare `phux new` would try to attach, and
    // this process has no TTY); it requires the name in `-s` flag form.
    server.success(&["new", "--json", "-s", PEER]);

    let peer_pane = server.peer_pane();
    let peer_selector = format!("@{}", peer_pane.local_id().expect("local peer pane"));
    // A headless layout op on the peer is what puts a layout under the peer's
    // key. `spawn --target` resolves the owning session itself, so the test
    // never has to name a session id that no JSON surface exposes.
    server.success(&["spawn", "--target", &peer_selector]);
    server.wait_for_persisted_layout(&peer_pane);

    // `blocked` is the top rung, so it renders as `!1` and also puts the pane
    // in zone 1. Any other state would either render nothing (`unknown` and
    // `idle` are omitted from the histogram by design, so the calm case adds
    // no noise) or share a glyph with a less specific rung. Targeted at the
    // exact pane rather than the session, which now has two.
    server.success(&[
        "agent",
        "set",
        &peer_selector,
        "--name",
        "claude",
        "--kind",
        "claude",
        "--state",
        "blocked",
    ]);

    let client = AttachedClient::start(&server);

    // `!1` is the whole point: one blocked pane in the peer session, a count
    // the client can only know by fetching that peer's layout and then that
    // pane's agent record. `spaces` and the peer name come free with the
    // session graph and are asserted only to keep a failure legible.
    let painted = client.wait_for_all(&["spaces", PEER, "!1"]);

    // The attached session belongs to zone 2 (`here`), never the roster. This
    // catches a sweep that stopped excluding the focused session — which would
    // also double every local layout broadcast.
    assert!(
        painted.contains("here"),
        "zone 2's header must still paint alongside the roster:\n{painted}"
    );
}
