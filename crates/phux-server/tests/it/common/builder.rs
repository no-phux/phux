//! `E2eBuilder` — the e2e flywheel harness.
//!
//! The hand-written tests in this crate all reimplement the same shape:
//! spin a server on a `LocalSet`, wait for the socket, attach one or more
//! clients, drain the `ATTACHED + TERMINAL_SNAPSHOT` opening sequence,
//! then loop `recv_typed` into a [`Screen`] oracle with a manual
//! `while deadline { match; if cond break }` body for every assertion.
//! That loop is the boilerplate this module deletes.
//!
//! A single client is modelled by [`ClientHandle`]: it owns the wire
//! [`UnixStream`], a [`Screen`] oracle fed by every drained
//! `TERMINAL_OUTPUT`, and the focused pane's [`TerminalId`] (so callers
//! send input without re-extracting it from the snapshot each time). The
//! handle exposes the verbs a repro actually wants:
//!
//!   * [`ClientHandle::send_text`] / [`ClientHandle::send_keys`] — push
//!     input as `INPUT_PASTE` (bulk text) or `INPUT_KEY` (named keys).
//!   * [`ClientHandle::screenshot`] — drain whatever output is already
//!     buffered (non-blocking) into the oracle and return it.
//!   * [`ClientHandle::wait_until`] — drain until a screen predicate holds.
//!   * [`ClientHandle::converge`] — drain until the screen stops changing
//!     for an idle window (the "screen settled" signal).
//!   * [`ClientHandle::resize`] — send `VIEWPORT_RESIZE`.
//!   * [`ClientHandle::detach`] / [`ClientHandle::reattach`] — drop the
//!     stream / open a fresh one against the same session.
//!
//! Everything is built on the existing [`crate::common`] helpers
//! (`spawn_server*`, `wait_for_socket`, `recv_typed`, `send_frame`) so a
//! regression that only shows over the wire still shows here.
//!
//! `!Send` note: the inner `Screen` owns a `!Send` libghostty `Terminal`
//! and the server runs on a `LocalSet`. Drive the builder from inside
//! [`crate::common::run_local`].

#![allow(
    clippy::future_not_send,
    reason = "harness futures run on a LocalSet; the inner Screen + server are !Send"
)]
#![allow(clippy::assigning_clones, reason = "tests: clarity over micro-opt")]

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use phux_protocol::TerminalId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_SNAPSHOT, ViewportInfo,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::screen::Screen;
use super::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, send_frame, spawn_server_with_seed_cmd,
    try_recv_typed, wait_for_socket,
};
use phux_server::ServerError;

/// Default oracle viewport. Matches [`attach_by_name`] (80x24) so the
/// `Screen` dimensions line up with the `ATTACH` the harness sends.
pub const DEFAULT_COLS: u16 = 80;
/// See [`DEFAULT_COLS`].
pub const DEFAULT_ROWS: u16 = 24;

/// How long [`ClientHandle::converge`] keeps draining after the last byte
/// before declaring the screen settled. A short window: the broadcast
/// pump emits within a couple of 33 Hz ticks, so 150ms of silence is a
/// confident "nothing more is coming."
pub const DEFAULT_IDLE_MS: u64 = 150;

/// Chainable spin-up for an e2e scenario. Collapses the
/// server-spawn + socket-wait + N-client-attach boilerplate into one
/// `run(|clients| async { ... })` call.
///
/// ```ignore
/// E2eBuilder::new()
///     .session("default")
///     .seed_cmd(CommandBuilder::new("/bin/cat"))
///     .clients(2)
///     .run(|mut clients| async move {
///         clients[0].send_text("hi\r").await;
///         clients[1].wait_until(|s| s.contains("hi")).await;
///     });
/// ```
pub struct E2eBuilder {
    session: String,
    seed_cmd: Option<CommandBuilder>,
    clients: usize,
    viewport: ViewportInfo,
}

impl Default for E2eBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl E2eBuilder {
    /// A fresh builder: session `default`, no seed command (a plain shell
    /// pane), one client, 80x24 viewport.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: "default".to_owned(),
            seed_cmd: None,
            clients: 1,
            viewport: ViewportInfo::new(DEFAULT_COLS, DEFAULT_ROWS),
        }
    }

    /// Set the pre-seeded session name every client attaches to.
    #[must_use]
    pub fn session(mut self, name: &str) -> Self {
        self.session = name.to_owned();
        self
    }

    /// Run `cmd` in the seed pane's PTY instead of the default shell. Use
    /// `/bin/cat` for a deterministic echo fixture or `/bin/sh -c '...'`
    /// for a scripted scenario.
    #[must_use]
    pub fn seed_cmd(mut self, cmd: CommandBuilder) -> Self {
        self.seed_cmd = Some(cmd);
        self
    }

    /// Number of clients to attach before the closure runs. Each gets its
    /// own [`ClientHandle`] in the `Vec` passed to [`Self::run`].
    #[must_use]
    pub fn clients(mut self, n: usize) -> Self {
        self.clients = n.max(1);
        self
    }

    /// Override the attach viewport (and the oracle dimensions). Defaults
    /// to 80x24.
    #[must_use]
    pub const fn viewport(mut self, cols: u16, rows: u16) -> Self {
        self.viewport = ViewportInfo::new(cols, rows);
        self
    }

    /// Spin the server, attach the requested clients, run `body`, then
    /// drive a clean shutdown and assert the socket was unlinked.
    ///
    /// MUST be called from inside [`crate::common::run_local`] (the server
    /// + oracle are `!Send`).
    ///
    /// # Panics
    /// Panics on any wire fault (a hung server, a malformed opening
    /// sequence, a teardown timeout) — a repro harness should fail loudly.
    pub async fn run<F, Fut>(self, body: F)
    where
        F: FnOnce(Vec<ClientHandle>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let harness = self.spawn().await;
        let Harness {
            clients,
            shutdown_tx,
            server_handle,
            socket_path,
            _tmp,
        } = harness;
        body(clients).await;
        shutdown_tx.send(()).ok();
        timeout(Duration::from_secs(5), server_handle)
            .await
            .expect("server did not shut down within 5s")
            .expect("server task join")
            .expect("server run_async returned an error");
        assert!(
            !socket_path.exists(),
            "socket file leaked after shutdown: {} still on disk",
            socket_path.display(),
        );
    }

    /// Lower-level entrypoint: spin the server + attach clients and return
    /// the live [`Harness`] without running a closure or tearing down.
    /// Use this when a test needs custom teardown timing (e.g. asserting
    /// the server self-exits) or wants to add clients mid-scenario.
    ///
    /// # Panics
    /// Panics if the socket never becomes connectable or any client's
    /// opening sequence is malformed.
    pub async fn spawn(self) -> Harness {
        let tmp = TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");

        // A seed command is required to get a real PTY-backed pane (the
        // no-PTY `spawn_server` path produces an empty grid). Default to a
        // login shell so a bare `E2eBuilder::new()` still yields an
        // interactive pane.
        let cmd = self
            .seed_cmd
            .unwrap_or_else(|| CommandBuilder::new(default_shell()));
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), &self.session, cmd);

        let mut clients = Vec::with_capacity(self.clients);
        for _ in 0..self.clients {
            clients.push(
                ClientHandle::attach(&socket_path, &self.session, self.viewport)
                    .await
                    .expect("client attach"),
            );
        }

        Harness {
            clients,
            shutdown_tx,
            server_handle,
            socket_path,
            _tmp: tmp,
        }
    }
}

/// A live harness: the attached clients plus the handles needed to tear
/// the server down. Held by value across a scenario.
pub struct Harness {
    /// One handle per attached client, in attach order.
    pub clients: Vec<ClientHandle>,
    /// Drives a clean server shutdown when sent.
    pub shutdown_tx: oneshot::Sender<()>,
    /// The server task; await it after shutdown to confirm a clean exit.
    pub server_handle: JoinHandle<Result<(), ServerError>>,
    /// The UDS path; assert `!exists()` after teardown to catch FD leaks.
    pub socket_path: PathBuf,
    /// Keeps the tempdir alive for the harness's lifetime.
    _tmp: TempDir,
}

impl Harness {
    /// Attach an additional client to the same session mid-scenario. Used
    /// by the attach/detach-churn stress test.
    ///
    /// # Panics
    /// Panics if the attach handshake is malformed or times out.
    pub async fn attach_client(&self, viewport: ViewportInfo) -> ClientHandle {
        ClientHandle::attach(&self.socket_path, &self.session_name(), viewport)
            .await
            .expect("attach additional client")
    }

    /// The session name a fresh client should attach to. Recovered from
    /// the first client's attach target so [`Self::attach_client`] needs
    /// no extra state.
    fn session_name(&self) -> String {
        self.clients
            .first()
            .map_or_else(|| "default".to_owned(), |c| c.session.clone())
    }

    /// Drive a clean server shutdown and assert the socket was unlinked.
    /// The mirror of [`E2eBuilder::run`]'s teardown for tests that drove a
    /// scenario via [`E2eBuilder::spawn`] and manage clients themselves.
    ///
    /// # Panics
    /// Panics if the server fails to shut down within 5s or the socket
    /// file leaks.
    pub async fn shutdown(self) {
        // Drop any live client streams first so the server's last
        // connection closes before we send the shutdown signal.
        drop(self.clients);
        self.shutdown_tx.send(()).ok();
        timeout(Duration::from_secs(5), self.server_handle)
            .await
            .expect("server did not shut down within 5s")
            .expect("server task join")
            .expect("server run_async returned an error");
        assert!(
            !self.socket_path.exists(),
            "socket file leaked after shutdown: {} still on disk",
            self.socket_path.display(),
        );
    }
}

/// One attached client: the wire stream, its oracle, and the focused
/// pane's id. All input/observe verbs hang off this.
pub struct ClientHandle {
    stream: UnixStream,
    screen: Screen,
    /// The focused pane's wire id, captured from the opening `ATTACHED`
    /// snapshot. Input frames target this terminal.
    pub terminal_id: TerminalId,
    /// Server-allocated client id for this attachment.
    pub client_id: u32,
    session: String,
    viewport: ViewportInfo,
    socket_path: PathBuf,
}

impl ClientHandle {
    /// Connect a fresh socket, attach to `session`, and drain the
    /// `ATTACHED + TERMINAL_SNAPSHOT` opening sequence into a fresh oracle.
    async fn attach(
        socket_path: &std::path::Path,
        session: &str,
        viewport: ViewportInfo,
    ) -> Result<Self, String> {
        let mut stream = wait_for_socket(socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut stream,
            &FrameKind::Attach {
                target: phux_protocol::wire::frame::AttachTarget::ByName(session.to_owned()),
                viewport,
                request_scrollback: false,
                scrollback_limit_lines: 0,
            },
        )
        .await;

        let (type_byte, attached) = recv_typed(&mut stream).await;
        if type_byte != TYPE_ATTACHED {
            return Err(format!(
                "expected ATTACHED (0x81), got type byte {type_byte:#x}"
            ));
        }
        let (client_id, terminal_id) = match attached {
            FrameKind::Attached {
                snapshot,
                initial_client_id,
            } => {
                let pane = snapshot
                    .panes
                    .first()
                    .ok_or_else(|| "ATTACHED snapshot had no panes".to_owned())?;
                (initial_client_id.get(), pane.id.clone())
            }
            other => return Err(format!("expected Attached, got {other:?}")),
        };

        // The opening snapshot for the focused pane. Feed it into the
        // oracle so `screenshot()` reflects initial state, not a blank
        // grid. There is one snapshot per pane in the focused window; for
        // the single-pane seed sessions this harness drives that is one
        // frame. Any extras are drained opportunistically below.
        let mut screen = Screen::new(viewport.cols, viewport.rows).expect("Screen::new");
        let (snap_tb, snap) = recv_typed(&mut stream).await;
        if snap_tb != TYPE_TERMINAL_SNAPSHOT {
            return Err(format!(
                "expected TERMINAL_SNAPSHOT (0x91), got type byte {snap_tb:#x}"
            ));
        }
        if let FrameKind::TerminalSnapshot {
            vt_replay_bytes, ..
        } = snap
        {
            screen.write(&vt_replay_bytes);
        }

        Ok(Self {
            stream,
            screen,
            terminal_id,
            client_id,
            session: session.to_owned(),
            viewport,
            socket_path: socket_path.to_owned(),
        })
    }

    /// Send `text` as an `INPUT_PASTE`. The bulk path: a single frame
    /// carries the whole string, which the server feeds to the PTY via
    /// `paste::encode` (bracketing decided by the pane's DEC 2004 state).
    /// Use this for typing strings and command lines; use
    /// [`Self::send_keys`] for keys that have no text (arrows, Ctrl-*).
    pub async fn send_text(&mut self, text: &str) {
        send_frame(
            &mut self.stream,
            &FrameKind::InputPaste {
                terminal_id: self.terminal_id.clone(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: text.as_bytes().to_vec(),
                },
            },
        )
        .await;
    }

    /// Send a sequence of named keys as individual `INPUT_KEY` frames.
    /// Each [`Key`] maps to a libghostty-atom `KeyEvent`. Use for control
    /// keys, arrows, Enter, etc.
    pub async fn send_keys(&mut self, keys: &[Key]) {
        for key in keys {
            send_frame(
                &mut self.stream,
                &FrameKind::InputKey {
                    terminal_id: self.terminal_id.clone(),
                    event: key.to_event(),
                },
            )
            .await;
        }
    }

    /// Convenience: type each char of `s` as a printable `INPUT_KEY`, then
    /// press Enter. Mirrors a user typing a command and hitting return —
    /// useful when the bracketed-paste path of [`Self::send_text`] would
    /// perturb the inner program (e.g. a shell that treats a paste
    /// differently from typed input).
    pub async fn type_line(&mut self, s: &str) {
        for ch in s.chars() {
            self.send_keys(&[Key::Char(ch)]).await;
        }
        self.send_keys(&[Key::Enter]).await;
    }

    /// Drain whatever `TERMINAL_OUTPUT` is *already* buffered on the wire
    /// into the oracle, without blocking for new output, then return a
    /// mutable view of the oracle. A non-blocking snapshot of "what the
    /// client would render right now."
    pub async fn screenshot(&mut self) -> &mut Screen {
        // Pull every frame that is immediately available. A zero-ish
        // timeout per recv keeps this from blocking on a quiet wire while
        // still consuming a frame that is mid-flight.
        loop {
            match timeout(Duration::from_millis(20), recv_typed(&mut self.stream)).await {
                Ok((tb, FrameKind::TerminalOutput { bytes, .. })) if tb == TYPE_TERMINAL_OUTPUT => {
                    self.screen.write(&bytes);
                }
                Ok(_) => {}      // non-output frame (bell, metadata, ack echo): ignore
                Err(_) => break, // no more immediately-available output
            }
        }
        &mut self.screen
    }

    /// Snapshot the oracle's current text WITHOUT draining the wire.
    ///
    /// Safe to call against a continuously-emitting seed where
    /// [`Self::screenshot`] would loop forever (its "drain until quiet" never
    /// terminates when output arrives faster than its idle window). Pair with
    /// [`Self::drain_output_bounded`] when the latest content is wanted.
    pub fn snapshot_text(&mut self) -> String {
        self.screen.snapshot_text()
    }

    /// Drain up to `max_frames` of immediately-available `TERMINAL_OUTPUT`
    /// into the oracle, stopping early on a brief (5ms) quiet gap.
    ///
    /// Unlike [`Self::screenshot`], this is BOUNDED by frame count, so it is
    /// safe to call against a seed that emits continuously (e.g. an infinite
    /// `printf` loop): `screenshot`'s "drain until quiet" never terminates
    /// when output arrives faster than its idle window. Use this inside a
    /// resize/output storm to keep the server's bounded outbound mailbox and
    /// socket buffer from filling — a client that only sends and never reads
    /// wedges the writer and deadlocks the shared current-thread runtime.
    pub async fn drain_output_bounded(&mut self, max_frames: usize) {
        for _ in 0..max_frames {
            match timeout(Duration::from_millis(5), recv_typed(&mut self.stream)).await {
                Ok((tb, FrameKind::TerminalOutput { bytes, .. })) if tb == TYPE_TERMINAL_OUTPUT => {
                    self.screen.write(&bytes);
                }
                Ok(_) => {}      // non-output frame: ignore, keep draining
                Err(_) => break, // brief quiet: backlog cleared for now
            }
        }
    }

    /// Drain `TERMINAL_OUTPUT` into the oracle until `pred` holds or
    /// [`WIRE_RECV_TIMEOUT`] elapses. Returns `Ok(())` if the predicate
    /// held, `Err` with the final screen text on timeout.
    ///
    /// This is the workhorse that replaces the hand-rolled
    /// `while deadline { recv_typed; match; if cond break }` loops.
    ///
    /// # Errors
    /// Returns the rendered screen text if the predicate never held before
    /// the deadline.
    pub async fn wait_until<P>(&mut self, pred: P) -> Result<(), String>
    where
        P: FnMut(&mut Screen) -> bool,
    {
        self.wait_until_with_timeout(WIRE_RECV_TIMEOUT, pred).await
    }

    /// [`wait_until`](Self::wait_until) with a caller-supplied deadline
    /// instead of the default [`WIRE_RECV_TIMEOUT`].
    ///
    /// Use this only for the rare test whose legitimate drain genuinely
    /// outlasts the standard budget on a constrained runner — e.g. a
    /// multi-megabyte no-newline burst whose single-thread reflow on two
    /// cores takes longer than 15s (phux-fheq). It is NOT a license to
    /// paper over a hung server: pick the smallest budget that covers the
    /// real work, and a stalled server still fails at the ceiling.
    ///
    /// # Errors
    /// Returns the rendered screen text if the predicate never held before
    /// the deadline.
    pub async fn wait_until_with_timeout<P>(
        &mut self,
        budget: Duration,
        mut pred: P,
    ) -> Result<(), String>
    where
        P: FnMut(&mut Screen) -> bool,
    {
        if pred(&mut self.screen) {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + budget;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            // phux-fheq: use the EOF-tolerant reader. Under the tmux
            // server-exit model a slow drain can race the server's
            // self-exit (it drops every client when its last session is
            // reaped), closing the socket mid-loop. That clean EOF is not a
            // test failure — break and report the screen we have, rather
            // than panicking with `UnexpectedEof` on the length prefix.
            match timeout(remaining, try_recv_typed(&mut self.stream)).await {
                Ok(Some((tb, frame))) => {
                    if tb == TYPE_TERMINAL_OUTPUT
                        && let FrameKind::TerminalOutput { bytes, .. } = frame
                    {
                        self.screen.write(&bytes);
                        if pred(&mut self.screen) {
                            return Ok(());
                        }
                    }
                }
                // Clean server close (self-exit) or outer timeout: stop.
                Ok(None) | Err(_) => break,
            }
        }
        Err(self.screen.snapshot_text())
    }

    /// Drain until the screen stops changing for `idle_ms` (the "settled"
    /// signal), or [`WIRE_RECV_TIMEOUT`] elapses. Returns the wall-clock
    /// time from the first drained byte to settle — the time-to-settle
    /// latency the perf gate measures.
    ///
    /// Two phases. First, wait up to [`WIRE_RECV_TIMEOUT`] for the FIRST
    /// `TERMINAL_OUTPUT` — the idle rule does NOT apply before any output
    /// arrives, so a deferred burst (e.g. a seed pane that sleeps before
    /// printing) is not mistaken for "already settled." Once output starts,
    /// the idle rule kicks in: when no further `TERMINAL_OUTPUT` arrives
    /// within `idle_ms`, the screen is settled. A long-running emitter (an
    /// infinite output loop) never settles and the call returns at the
    /// [`WIRE_RECV_TIMEOUT`] ceiling.
    pub async fn converge(&mut self, idle_ms: u64) -> Duration {
        let idle = Duration::from_millis(idle_ms);
        let hard_deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut first_byte_at: Option<Instant> = None;
        loop {
            let now = tokio::time::Instant::now();
            if now >= hard_deadline {
                break;
            }
            // Before the first byte, wait the full remaining budget (a
            // deferred burst must not look settled). After it, only wait
            // out the idle window.
            let budget = if first_byte_at.is_some() {
                (hard_deadline - now).min(idle)
            } else {
                hard_deadline - now
            };
            match timeout(budget, recv_typed(&mut self.stream)).await {
                Ok((tb, FrameKind::TerminalOutput { bytes, .. })) if tb == TYPE_TERMINAL_OUTPUT => {
                    first_byte_at.get_or_insert_with(Instant::now);
                    self.screen.write(&bytes);
                }
                Ok(_) => {}      // non-output frame; not a screen change, keep waiting
                Err(_) => break, // idle window (or first-byte wait) elapsed: settled
            }
        }
        // Time-to-settle is measured from the first byte (input->render
        // latency). If nothing ever arrived, report zero rather than the
        // full first-byte wait (no output means no latency to gate).
        first_byte_at.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Send a `VIEWPORT_RESIZE`. Updates the oracle dimensions to match so
    /// subsequent `screenshot()` reads reflect the new geometry. Note the
    /// oracle is rebuilt fresh, so prior content is dropped — callers that
    /// care should `converge` after a resize to repopulate from the
    /// server's reflowed output.
    pub async fn resize(&mut self, cols: u16, rows: u16) {
        self.viewport = ViewportInfo::new(cols, rows);
        self.screen = Screen::new(cols, rows).expect("Screen::new on resize");
        send_frame(
            &mut self.stream,
            &FrameKind::ViewportResize {
                viewport: self.viewport,
            },
        )
        .await;
    }

    /// Send a `VIEWPORT_RESIZE` with the EXACT requested dimensions over
    /// the wire (including degenerate `0`/extreme values) WITHOUT rebuilding
    /// the oracle to those dims — the oracle has no concept of a
    /// zero-dimension grid, so it is clamped to a 1-cell minimum here. Use
    /// this in crash-hunt scenarios that need to push pathological viewports
    /// at the server; use [`Self::resize`] for normal geometry where the
    /// oracle should track the new size.
    pub async fn resize_raw(&mut self, cols: u16, rows: u16) {
        self.viewport = ViewportInfo::new(cols, rows);
        self.screen =
            Screen::new(cols.max(1), rows.max(1)).expect("Screen::new on raw resize (clamped)");
        send_frame(
            &mut self.stream,
            &FrameKind::ViewportResize {
                viewport: self.viewport,
            },
        )
        .await;
    }

    /// Detach by dropping the wire stream (a hard client departure). The
    /// server reaps the connection on EOF. Consumes the handle's stream;
    /// call [`Self::reattach`]-style flows via the [`Harness`] instead, or
    /// use [`Self::send_detach`] for a graceful `DETACH`.
    pub fn detach(self) {
        drop(self.stream);
    }

    /// Send a graceful `DETACH` frame (the server replies `DETACHED` and
    /// closes). Leaves the handle intact so a test can assert on the
    /// `DETACHED`/EOF afterward.
    pub async fn send_detach(&mut self) {
        send_frame(&mut self.stream, &FrameKind::Detach).await;
    }

    /// Open a fresh connection to the same session and return a new
    /// handle, leaving `self` untouched. Models a client reconnecting
    /// (e.g. after a network blip) without losing the original.
    ///
    /// # Panics
    /// Panics if the re-attach handshake is malformed or times out.
    pub async fn reattach(&self) -> Self {
        Self::attach(&self.socket_path, &self.session, self.viewport)
            .await
            .expect("reattach")
    }
}

/// Pick a deterministic, banner-free shell for seed panes. `/bin/sh`
/// avoids the interactive-shell rc noise (p10k, direnv) that would
/// pollute screenshot assertions.
fn default_shell() -> String {
    "/bin/sh".to_owned()
}

/// A scripted, deterministic HEAVY-COLORED-output seed command — the
/// repro for the user's actual lag symptom (zsh completion menu /
/// syntax-highlighted scroll: many full-width SGR-laden rows rewritten
/// every frame).
///
/// Shape: `gens` repaints of a `rows`-tall screen, where every row is a
/// full `cols`-wide line built from per-cell `\033[38;5;Nm` 256-color
/// SGR runs — i.e. an SGR change roughly every other column, the
/// worst-case churn for the per-consumer diff and the client's VT apply +
/// per-cell render. Each repaint homes the cursor (`\033[H`) so the whole
/// grid is rewritten in place (no scroll), matching a live completion
/// menu redrawing on each keystroke. The shell sleeps briefly first so
/// clients attach before the burst lands as a live delta, prints
/// `COLORDONE` as a settle marker, then idles so the connection stays
/// open for teardown.
///
/// Deterministic: no RNG, fixed iteration counts, color index a pure
/// function of `(row, col, gen)`. Two runs emit identical bytes, so the
/// gate's settle assertion is reproducible.
#[must_use]
pub fn colored_burst_command(cols: u16, rows: u16, gens: u16) -> CommandBuilder {
    // Each row: for each cell column, switch to a 256-color foreground
    // then print one visible glyph. `printf` in a tight inner loop is too
    // slow at this density, so build each row's bytes once per (row,gen)
    // with a column loop that appends to a shell variable, then print the
    // whole row in one `printf`. Color index cycles through the 216-color
    // cube (16..=231) as a function of column+row+gen so adjacent cells
    // differ (forcing an SGR delta per cell, the heavy case).
    let script = format!(
        "sleep 0.3; \
         cols={cols}; rows={rows}; \
         for g in $(seq 1 {gens}); do \
           printf '\\033[H'; \
           r=1; \
           while [ \"$r\" -le \"$rows\" ]; do \
             line=''; c=1; \
             while [ \"$c\" -le \"$cols\" ]; do \
               n=$(( 16 + (c + r + g) % 216 )); \
               line=\"$line\\033[38;5;${{n}}mX\"; \
               c=$(( c + 1 )); \
             done; \
             printf \"%b\\033[0m\\r\\n\" \"$line\"; \
             r=$(( r + 1 )); \
           done; \
         done; \
         printf '\\033[0mCOLORDONE\\r\\n'; sleep 30"
    );
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", &script]);
    cmd
}

/// A named key for [`ClientHandle::send_keys`]. Covers the keys a repro
/// actually drives; falls through to [`Key::Char`] for printables. The
/// mapping to a [`KeyEvent`] mirrors the `ascii_key`/`enter_key` helpers
/// the hand-written tests define.
#[derive(Debug, Clone, Copy)]
pub enum Key {
    /// A printable character (its own `text` + unshifted codepoint).
    Char(char),
    /// Return / Enter.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Arrow up/down/left/right.
    Up,
    /// See [`Key::Up`].
    Down,
    /// See [`Key::Up`].
    Left,
    /// See [`Key::Up`].
    Right,
    /// Ctrl + an ASCII letter (e.g. `Ctrl('c')` for SIGINT).
    Ctrl(char),
}

impl Key {
    /// Lower a named key into a wire [`KeyEvent`].
    fn to_event(self) -> KeyEvent {
        let press =
            |key: PhysicalKey, mods: ModSet, text: Option<String>, cp: Option<u32>| KeyEvent {
                action: KeyAction::Press,
                key,
                mods,
                consumed_mods: ModSet::empty(),
                composing: false,
                text,
                unshifted_codepoint: cp,
            };
        match self {
            Self::Char(c) => press(
                physical_for_char(c),
                ModSet::empty(),
                Some(c.to_string()),
                Some(c as u32),
            ),
            Self::Enter => press(PhysicalKey::Enter, ModSet::empty(), None, None),
            Self::Tab => press(PhysicalKey::Tab, ModSet::empty(), None, None),
            Self::Esc => press(PhysicalKey::Escape, ModSet::empty(), None, None),
            Self::Backspace => press(PhysicalKey::Backspace, ModSet::empty(), None, None),
            Self::Up => press(PhysicalKey::ArrowUp, ModSet::empty(), None, None),
            Self::Down => press(PhysicalKey::ArrowDown, ModSet::empty(), None, None),
            Self::Left => press(PhysicalKey::ArrowLeft, ModSet::empty(), None, None),
            Self::Right => press(PhysicalKey::ArrowRight, ModSet::empty(), None, None),
            Self::Ctrl(c) => {
                let lower = c.to_ascii_lowercase();
                press(
                    physical_for_char(lower),
                    ModSet::CTRL,
                    None,
                    Some(lower as u32),
                )
            }
        }
    }
}

/// Map an ASCII char to its W3C physical key code. Letters and digits are
/// covered; anything else degrades to [`PhysicalKey::Unidentified`] (the
/// `text` field still carries the character, so printables still type).
const fn physical_for_char(c: char) -> PhysicalKey {
    match c.to_ascii_lowercase() {
        'a' => PhysicalKey::A,
        'b' => PhysicalKey::B,
        'c' => PhysicalKey::C,
        'd' => PhysicalKey::D,
        'e' => PhysicalKey::E,
        'f' => PhysicalKey::F,
        'g' => PhysicalKey::G,
        'h' => PhysicalKey::H,
        'i' => PhysicalKey::I,
        'j' => PhysicalKey::J,
        'k' => PhysicalKey::K,
        'l' => PhysicalKey::L,
        'm' => PhysicalKey::M,
        'n' => PhysicalKey::N,
        'o' => PhysicalKey::O,
        'p' => PhysicalKey::P,
        'q' => PhysicalKey::Q,
        'r' => PhysicalKey::R,
        's' => PhysicalKey::S,
        't' => PhysicalKey::T,
        'u' => PhysicalKey::U,
        'v' => PhysicalKey::V,
        'w' => PhysicalKey::W,
        'x' => PhysicalKey::X,
        'y' => PhysicalKey::Y,
        'z' => PhysicalKey::Z,
        '0' => PhysicalKey::Digit0,
        '1' => PhysicalKey::Digit1,
        '2' => PhysicalKey::Digit2,
        '3' => PhysicalKey::Digit3,
        '4' => PhysicalKey::Digit4,
        '5' => PhysicalKey::Digit5,
        '6' => PhysicalKey::Digit6,
        '7' => PhysicalKey::Digit7,
        '8' => PhysicalKey::Digit8,
        '9' => PhysicalKey::Digit9,
        ' ' => PhysicalKey::Space,
        _ => PhysicalKey::Unidentified,
    }
}

// ---------------------------------------------------------------------------
// Timed input-sequence replay (item 2).
// ---------------------------------------------------------------------------

/// One step in a timed input script: wait `delay`, then deliver `input`.
#[derive(Debug, Clone)]
pub struct ScriptStep {
    /// How long to sleep before this step's input is sent.
    pub delay: Duration,
    /// The input to deliver.
    pub input: ScriptInput,
}

/// The payload of a [`ScriptStep`].
#[derive(Debug, Clone)]
pub enum ScriptInput {
    /// Bulk text via `INPUT_PASTE`.
    Text(String),
    /// A sequence of named keys via `INPUT_KEY`.
    Keys(Vec<Key>),
    /// A viewport resize.
    Resize(u16, u16),
}

/// A timed input script: an ordered list of `(delay, input)` steps. The
/// replay driver sleeps the delay then delivers each step against a
/// [`ClientHandle`], so a lag repro reads as a literal timeline.
#[derive(Debug, Clone, Default)]
pub struct InputScript {
    steps: Vec<ScriptStep>,
}

impl InputScript {
    /// An empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a `delay`-then-text step.
    #[must_use]
    pub fn text(mut self, delay: Duration, text: &str) -> Self {
        self.steps.push(ScriptStep {
            delay,
            input: ScriptInput::Text(text.to_owned()),
        });
        self
    }

    /// Append a `delay`-then-keys step.
    #[must_use]
    pub fn keys(mut self, delay: Duration, keys: Vec<Key>) -> Self {
        self.steps.push(ScriptStep {
            delay,
            input: ScriptInput::Keys(keys),
        });
        self
    }

    /// Append a `delay`-then-resize step.
    #[must_use]
    pub fn resize(mut self, delay: Duration, cols: u16, rows: u16) -> Self {
        self.steps.push(ScriptStep {
            delay,
            input: ScriptInput::Resize(cols, rows),
        });
        self
    }

    /// The number of steps queued.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the script has no steps.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Replay the script against `client`, honouring each step's delay.
    /// Does NOT drain output between steps — call
    /// [`ClientHandle::converge`] or [`ClientHandle::wait_until`] after to
    /// observe the result, or interleave manually for tighter timing.
    pub async fn replay(&self, client: &mut ClientHandle) {
        for step in &self.steps {
            if !step.delay.is_zero() {
                tokio::time::sleep(step.delay).await;
            }
            match &step.input {
                ScriptInput::Text(t) => client.send_text(t).await,
                ScriptInput::Keys(keys) => client.send_keys(keys).await,
                ScriptInput::Resize(c, r) => client.resize(*c, *r).await,
            }
        }
    }
}
