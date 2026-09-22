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
//! [`UnixStream`], a replica-aware [`Screen`] oracle fed by every drained
//! render frame, and the focused pane's [`ResourceId`] (so callers
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
//!   * [`ClientHandle::converge_until`] — same, but ignore idle gaps until
//!     a completion predicate holds.
//!   * [`ClientHandle::converge_until_with_timeout`] — same, with a caller
//!     deadline (the colored perf gate's ceiling exceeds the default).
//!   * [`ClientHandle::resize`] — send `VIEWPORT_RESIZE`.
//!   * [`ClientHandle::detach`] / [`ClientHandle::reattach`] — drop the
//!     stream / open a fresh one against the same session.
//!
//! Everything is built on the existing [`crate`] helpers
//! (`spawn_server*`, `wait_for_socket`, `recv_typed`, `send_frame`) so a
//! regression that only shows over the wire still shows here.
//!
//! `!Send` note: the inner `Screen` owns a `!Send` libghostty `Terminal`
//! and the server runs on a `LocalSet`. Drive the builder from inside
//! [`crate::run_local`].

#![allow(
    clippy::future_not_send,
    reason = "harness futures run on a LocalSet; the inner Screen + server are !Send"
)]
#![allow(clippy::assigning_clones, reason = "tests: clarity over micro-opt")]

use std::collections::HashSet;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    ViewportInfo,
};
use phux_protocol::{ResourceId, wire::frame::MAX_FRAME_LEN};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at};

use super::screen::Screen;
use super::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, send_frame, spawn_server_with_seed_cmd,
    wait_for_socket,
};
use phux_server::ServerError;

/// Default oracle viewport. Matches [`crate::attach_by_name`] (80x24) so the
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
    /// MUST be called from inside [`crate::run_local`] (the server
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
        timeout(super::SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
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
        // plain interactive shell (not a login shell — see phux-87rr's
        // `login_flag_for_shell` for what that distinction now means in
        // this codebase) so a bare `E2eBuilder::new()` still yields an
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
        timeout(super::SERVER_JOIN_DEADLINE, self.server_handle)
            .await
            .expect("server did not shut down after the shutdown signal")
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
    receiver: BufferedFrameReceiver,
    oracle: ScreenOracle,
    /// The focused pane's wire id, captured from the opening `ATTACHED`
    /// snapshot. Input frames target this terminal.
    pub terminal_id: ResourceId,
    /// Server-allocated client id for this attachment.
    pub client_id: u32,
    session: String,
    viewport: ViewportInfo,
    socket_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReplicaGeneration {
    terminal: ResourceId,
    stream: StreamId,
    bootstrap: BootstrapId,
}

impl ReplicaGeneration {
    fn new(terminal_id: &ResourceId, stream_id: StreamId, bootstrap_id: BootstrapId) -> Self {
        Self {
            terminal: terminal_id.clone(),
            stream: stream_id,
            bootstrap: bootstrap_id,
        }
    }
}

#[derive(Debug)]
struct ReplicaScreen {
    generation: ReplicaGeneration,
    screen: Screen,
    next_output_seq: Option<u64>,
}

#[derive(Debug)]
struct StagedScreen {
    replica: ReplicaScreen,
    next_chunk_seq: Option<u32>,
}

/// Minimal terminal-replica state needed by the testkit's rendered-screen
/// oracle. Replacement bootstraps render off-screen and publish atomically at
/// their matching `BOOTSTRAP_READY`, mirroring the production client kernel.
#[derive(Debug)]
struct ScreenOracle {
    terminal_id: ResourceId,
    published: Option<ReplicaScreen>,
    staging: Option<StagedScreen>,
    retired: HashSet<ReplicaGeneration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppliedFrame {
    Ignored,
    RenderedStaging,
    RenderedPublished,
    PublishedReplacement,
}

impl AppliedFrame {
    const fn rendered(self) -> bool {
        matches!(self, Self::RenderedStaging | Self::RenderedPublished)
    }

    const fn published_changed(self) -> bool {
        matches!(self, Self::RenderedPublished | Self::PublishedReplacement)
    }
}

impl ScreenOracle {
    fn new(terminal_id: ResourceId) -> Self {
        Self {
            terminal_id,
            published: None,
            staging: None,
            retired: HashSet::new(),
        }
    }

    const fn screen_mut(&mut self) -> &mut Screen {
        &mut self
            .published
            .as_mut()
            .expect("screen oracle has no published bootstrap generation")
            .screen
    }

    const fn has_published(&self) -> bool {
        self.published.is_some()
    }

    const fn has_staging(&self) -> bool {
        self.staging.is_some()
    }

    fn replace_screen(&mut self, cols: u16, rows: u16) {
        self.published
            .as_mut()
            .expect("screen oracle has no published bootstrap generation")
            .screen = Screen::new(cols, rows).expect("Screen::new on resize");
    }

    fn apply(&mut self, frame: &FrameKind) -> AppliedFrame {
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => self.begin(
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *profile,
                *cols,
                *rows,
                *base_seq,
            ),
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => self.chunk(terminal_id, *stream_id, *bootstrap_id, *chunk_seq, payload),
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => self.ready(terminal_id, *stream_id, *bootstrap_id),
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => self.output(terminal_id, *stream_id, *bootstrap_id, *seq, bytes),
            _ => AppliedFrame::Ignored,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the helper validates the BOOTSTRAP_BEGIN wire fields without duplicating them"
    )]
    fn begin(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        cols: u16,
        rows: u16,
        base_seq: u64,
    ) -> AppliedFrame {
        let generation = ReplicaGeneration::new(terminal_id, stream_id, bootstrap_id);
        let profile_is_vt = matches!(
            profile,
            BootstrapStreamProfile::SynthesizedVtRaw
                | BootstrapStreamProfile::SynthesizedVtStateSync
        );
        let is_current = self
            .published
            .as_ref()
            .is_some_and(|replica| replica.generation == generation);
        let is_staging = self
            .staging
            .as_ref()
            .is_some_and(|staging| staging.replica.generation == generation);
        if terminal_id != &self.terminal_id
            || cols == 0
            || rows == 0
            || !profile_is_vt
            || is_current
            || is_staging
            || self.retired.contains(&generation)
        {
            return AppliedFrame::Ignored;
        }

        if let Some(replaced) = self.staging.take() {
            self.retired.insert(replaced.replica.generation);
        }
        self.staging = Some(StagedScreen {
            replica: ReplicaScreen {
                generation,
                screen: Screen::new(cols, rows).expect("Screen::new for bootstrap"),
                next_output_seq: base_seq.checked_add(1),
            },
            next_chunk_seq: Some(0),
        });
        AppliedFrame::Ignored
    }

    fn chunk(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
    ) -> AppliedFrame {
        let generation = ReplicaGeneration::new(terminal_id, stream_id, bootstrap_id);
        let Some(staging) = self
            .staging
            .as_mut()
            .filter(|staging| staging.replica.generation == generation)
        else {
            return AppliedFrame::Ignored;
        };
        let Some(expected) = staging.next_chunk_seq else {
            panic!("bootstrap chunk sequence exhausted for matching generation");
        };
        match chunk_seq.cmp(&expected) {
            std::cmp::Ordering::Less => {
                panic!("duplicate bootstrap chunk: expected {expected}, got {chunk_seq}")
            }
            std::cmp::Ordering::Greater => {
                panic!("bootstrap chunk gap: expected {expected}, got {chunk_seq}")
            }
            std::cmp::Ordering::Equal => {}
        }
        staging.replica.screen.write(payload);
        staging.next_chunk_seq = chunk_seq.checked_add(1);
        AppliedFrame::RenderedStaging
    }

    fn ready(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> AppliedFrame {
        let generation = ReplicaGeneration::new(terminal_id, stream_id, bootstrap_id);
        let Some(staging) = self
            .staging
            .take_if(|staging| staging.replica.generation == generation)
        else {
            return AppliedFrame::Ignored;
        };
        if let Some(replaced) = self.published.replace(staging.replica) {
            self.retired.insert(replaced.generation);
        }
        AppliedFrame::PublishedReplacement
    }

    fn output(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        bytes: &[u8],
    ) -> AppliedFrame {
        let generation = ReplicaGeneration::new(terminal_id, stream_id, bootstrap_id);
        let Some(published) = self
            .published
            .as_mut()
            .filter(|published| published.generation == generation)
        else {
            return AppliedFrame::Ignored;
        };
        let Some(expected) = published.next_output_seq else {
            panic!("live output sequence exhausted for matching generation");
        };
        match seq.cmp(&expected) {
            std::cmp::Ordering::Less => {
                panic!("duplicate live output: expected {expected}, got {seq}")
            }
            std::cmp::Ordering::Greater => {
                panic!("live output gap: expected {expected}, got {seq}")
            }
            std::cmp::Ordering::Equal => {}
        }
        published.screen.write(bytes);
        published.next_output_seq = seq.checked_add(1);
        AppliedFrame::RenderedPublished
    }
}

#[derive(Debug)]
struct BufferedFrameReceiver {
    buffered: BytesMut,
    scratch: Vec<u8>,
}

impl Default for BufferedFrameReceiver {
    fn default() -> Self {
        Self {
            buffered: BytesMut::new(),
            scratch: vec![0; 16 * 1024],
        }
    }
}

#[derive(Debug)]
enum ReceiveBefore {
    Frame(FrameKind),
    Eof,
    Deadline,
}

impl BufferedFrameReceiver {
    async fn receive_before(
        &mut self,
        stream: &mut UnixStream,
        deadline: tokio::time::Instant,
    ) -> ReceiveBefore {
        loop {
            if tokio::time::Instant::now() >= deadline {
                return ReceiveBefore::Deadline;
            }
            if let Some(frame) = self.decode_buffered() {
                return ReceiveBefore::Frame(frame);
            }

            // `AsyncReadExt::read` is cancellation-safe. If the deadline wins,
            // no bytes are lost; bytes from earlier reads remain in `buffered`
            // for the next caller instead of abandoning a partial frame.
            let read = match timeout_at(deadline, stream.read(&mut self.scratch)).await {
                Ok(Ok(read)) => read,
                Ok(Err(error)) => panic!("recv frame io error: {error}"),
                Err(_) => return ReceiveBefore::Deadline,
            };
            if read == 0 {
                assert!(
                    self.buffered.is_empty(),
                    "connection closed in the middle of a wire frame"
                );
                return ReceiveBefore::Eof;
            }
            self.buffered.extend_from_slice(&self.scratch[..read]);
        }
    }

    fn decode_buffered(&mut self) -> Option<FrameKind> {
        let header: [u8; 4] = self
            .buffered
            .get(..4)?
            .try_into()
            .expect("four-byte header");
        let body_len = u32::from_be_bytes(header);
        assert!(
            (1..=MAX_FRAME_LEN).contains(&body_len),
            "wire frame length {body_len} is outside 1..={MAX_FRAME_LEN}"
        );
        let framed_len = 4 + usize::try_from(body_len).expect("u32 fits usize");
        if self.buffered.len() < framed_len {
            return None;
        }
        let framed = self.buffered.split_to(framed_len);
        let (frame, rest) = FrameKind::decode(&framed).expect("decode frame");
        assert!(rest.is_empty(), "decoder did not consume entire frame");
        Some(frame)
    }
}

async fn receive_opening_oracle(
    stream: &mut UnixStream,
    terminal_id: &ResourceId,
) -> Result<ScreenOracle, String> {
    let mut oracle = ScreenOracle::new(terminal_id.clone());
    let (begin_type, begin) = recv_typed(stream).await;
    if begin_type != TYPE_BOOTSTRAP_BEGIN {
        return Err(format!("expected BOOTSTRAP_BEGIN, got {begin_type:#x}"));
    }
    let FrameKind::BootstrapBegin {
        stream_id,
        bootstrap_id,
        ..
    } = &begin
    else {
        return Err(format!("expected BootstrapBegin, got {begin:?}"));
    };
    let generation = (*stream_id, *bootstrap_id);
    let _ = oracle.apply(&begin);

    loop {
        let (type_byte, frame) = recv_typed(stream).await;
        let matching_chunk = matches!(
            &frame,
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            } if type_byte == TYPE_BOOTSTRAP_CHUNK && (*stream_id, *bootstrap_id) == generation
        );
        let matching_ready = matches!(
            &frame,
            FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } if type_byte == TYPE_BOOTSTRAP_READY && (*stream_id, *bootstrap_id) == generation
        );
        if !matching_chunk && !matching_ready {
            return Err(format!(
                "expected matching BOOTSTRAP_CHUNK/READY, got {frame:?}"
            ));
        }
        let _ = oracle.apply(&frame);
        if matching_ready {
            return oracle
                .has_published()
                .then_some(oracle)
                .ok_or_else(|| "bootstrap READY did not publish a screen generation".to_owned());
        }
    }
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
                attach_id: 1,
                target: phux_protocol::wire::frame::AttachTarget::ByName(session.to_owned()),
                viewport,
                request_scrollback: false,
                scrollback_limit_lines: 0,
                role_policy: None,
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
                attach_id,
                snapshot,
                initial_client_id,
            } => {
                assert_eq!(attach_id, 1, "ATTACHED must echo ATTACH.attach_id");
                let pane = snapshot
                    .resources
                    .first()
                    .ok_or_else(|| "ATTACHED snapshot had no panes".to_owned())?;
                (initial_client_id.get(), pane.id.clone())
            }
            other => return Err(format!("expected Attached, got {other:?}")),
        };

        // Drain BEGIN -> CHUNK* -> READY through the same replica-aware seam
        // used by every post-attach screen helper.
        let oracle = receive_opening_oracle(&mut stream, &terminal_id).await?;

        Ok(Self {
            stream,
            receiver: BufferedFrameReceiver::default(),
            oracle,
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

    /// Drain whatever terminal render frames are *already* buffered on the
    /// wire into the oracle, without blocking for new output, then return a
    /// mutable view of the oracle. A non-blocking snapshot of "what the
    /// client would render right now."
    pub async fn screenshot(&mut self) -> &mut Screen {
        // Pull every frame that is immediately available. A zero-ish
        // timeout per recv keeps this from blocking on a quiet wire while
        // still consuming a frame that is mid-flight.
        loop {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(20);
            match self.receive_before(deadline).await {
                ReceiveBefore::Frame(frame) => {
                    let _ = self.oracle.apply(&frame);
                }
                ReceiveBefore::Eof | ReceiveBefore::Deadline => break,
            }
        }
        self.oracle.screen_mut()
    }

    /// Snapshot the oracle's current text WITHOUT draining the wire.
    ///
    /// Safe to call against a continuously-emitting seed where
    /// [`Self::screenshot`] would loop forever (its "drain until quiet" never
    /// terminates when output arrives faster than its idle window). Pair with
    /// [`Self::drain_output_bounded`] when the latest content is wanted.
    pub fn snapshot_text(&mut self) -> String {
        self.oracle.screen_mut().snapshot_text()
    }

    /// Drain up to `max_frames` of immediately-available wire frames into the
    /// oracle, stopping early on a brief (5ms) quiet gap.
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
            let deadline = tokio::time::Instant::now() + Duration::from_millis(5);
            match self.receive_before(deadline).await {
                ReceiveBefore::Frame(frame) => {
                    let _ = self.oracle.apply(&frame);
                }
                ReceiveBefore::Eof | ReceiveBefore::Deadline => break,
            }
        }
    }

    /// Drain terminal render frames into the oracle until `pred` holds or
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
        if pred(self.oracle.screen_mut()) {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + budget;
        while tokio::time::Instant::now() < deadline {
            // phux-fheq: use the EOF-tolerant reader. Under the tmux
            // server-exit model a slow drain can race the server's
            // self-exit (it drops every client when its last session is
            // reaped), closing the socket mid-loop. That clean EOF is not a
            // test failure — break and report the screen we have, rather
            // than panicking with `UnexpectedEof` on the length prefix.
            match self.receive_before(deadline).await {
                ReceiveBefore::Frame(frame) => {
                    if self.oracle.apply(&frame).published_changed()
                        && pred(self.oracle.screen_mut())
                    {
                        return Ok(());
                    }
                }
                ReceiveBefore::Eof | ReceiveBefore::Deadline => break,
            }
        }
        Err(self.oracle.screen_mut().snapshot_text())
    }

    /// Drain until the screen stops changing for `idle_ms` (the "settled"
    /// signal), or [`WIRE_RECV_TIMEOUT`] elapses. Returns the wall-clock
    /// time from the first drained byte to settle — the time-to-settle
    /// latency the perf gate measures.
    ///
    /// Two phases. First, wait up to [`WIRE_RECV_TIMEOUT`] for the FIRST
    /// applied output/bootstrap payload — the idle rule does NOT apply before
    /// any output arrives, so a deferred burst (e.g. a seed pane that sleeps
    /// before printing) is not mistaken for "already settled." Once output starts,
    /// the idle rule kicks in: when no further rendered payload arrives
    /// within `idle_ms`, the screen is settled. A long-running emitter (an
    /// infinite output loop) never settles and the call returns at the
    /// [`WIRE_RECV_TIMEOUT`] ceiling.
    ///
    /// Idle-only settle is the wrong oracle for a burst that can pause
    /// between rows. Use [`Self::converge_until`] when completion has a
    /// marker.
    pub async fn converge(&mut self, idle_ms: u64) -> Duration {
        self.converge_until(idle_ms, |_| true).await
    }

    /// [`converge`](Self::converge), but a quiet gap is not settle until
    /// `pred` holds.
    ///
    /// A burst that can pause between rows (or a loaded host that
    /// schedules the emitter in fits) can go quiet for longer than
    /// [`DEFAULT_IDLE_MS`] before its completion marker. Treating that gap
    /// as settle makes the latency gate race the emitter (phux-4s38). The
    /// idle window starts only after the predicate is true; first-byte
    /// timing is unchanged.
    pub async fn converge_until<P>(&mut self, idle_ms: u64, pred: P) -> Duration
    where
        P: FnMut(&mut Screen) -> bool,
    {
        self.converge_until_with_timeout(idle_ms, WIRE_RECV_TIMEOUT, pred)
            .await
    }

    /// [`converge_until`](Self::converge_until) with a caller-supplied hard
    /// deadline instead of [`WIRE_RECV_TIMEOUT`].
    ///
    /// Use this when the legitimate drain can outlast the standard 15s
    /// budget — the colored-output perf gate's settle ceiling is 30s, and
    /// waiting only 15s reported "never completed" under host load
    /// (phux-iuxr). Same rule as [`Self::wait_until_with_timeout`]: pick
    /// the smallest budget that covers the real work; a stalled server
    /// still fails at the ceiling.
    pub async fn converge_until_with_timeout<P>(
        &mut self,
        idle_ms: u64,
        budget: Duration,
        mut pred: P,
    ) -> Duration
    where
        P: FnMut(&mut Screen) -> bool,
    {
        let idle = Duration::from_millis(idle_ms);
        let hard_deadline = tokio::time::Instant::now() + budget;
        let mut first_byte_at: Option<Instant> = None;
        let mut last_render_at: Option<tokio::time::Instant> = None;
        let mut complete = pred(self.oracle.screen_mut());
        loop {
            let now = tokio::time::Instant::now();
            if now >= hard_deadline {
                break;
            }
            // Before the first byte, or before the completion marker, wait
            // the remaining hard budget so a transient gap is not settle.
            // After both, only wait out the idle window.
            let receive_deadline = if complete {
                last_render_at
                    .map_or(hard_deadline, |last| last + idle)
                    .min(hard_deadline)
            } else {
                hard_deadline
            };
            match self.receive_before(receive_deadline).await {
                ReceiveBefore::Frame(frame) => {
                    let applied = self.oracle.apply(&frame);
                    let applied_at = tokio::time::Instant::now();
                    if applied.rendered() {
                        first_byte_at.get_or_insert_with(Instant::now);
                        last_render_at = Some(applied_at);
                    }
                    if applied.published_changed() {
                        // Publishing a staged replacement changes the visible
                        // screen even though READY carries no VT payload.
                        last_render_at = Some(applied_at);
                    }
                    complete = if self.oracle.has_staging() {
                        false
                    } else if applied.published_changed() {
                        pred(self.oracle.screen_mut())
                    } else {
                        complete
                    };
                }
                ReceiveBefore::Eof | ReceiveBefore::Deadline => break,
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
        self.oracle.replace_screen(cols, rows);
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
        self.oracle.replace_screen(cols.max(1), rows.max(1));
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

    async fn receive_before(&mut self, deadline: tokio::time::Instant) -> ReceiveBefore {
        self.receiver
            .receive_before(&mut self.stream, deadline)
            .await
    }
}

/// Pick a deterministic, banner-free shell for seed panes. `/bin/sh`
/// avoids the interactive-shell rc noise (p10k, direnv) that would
/// pollute screenshot assertions.
fn default_shell() -> String {
    "/bin/sh".to_owned()
}

/// Bytes for the heavy-colored burst the perf gate drives.
///
/// `gens` full-grid repaints of `rows` × `cols`, each cell its own
/// 256-color SGR (`\033[38;5;NmX`), homed with `\033[H` so the grid is
/// rewritten in place. Color index is `16 + (col + row + gen) % 216`.
/// Ends with `COLORDONE`. No RNG; two calls with the same geometry emit
/// identical bytes.
#[must_use]
pub fn colored_burst_bytes(cols: u16, rows: u16, gens: u16) -> Vec<u8> {
    // Per-cell SGR is ~12 bytes; 80×40×24 is under a megabyte.
    let mut out =
        Vec::with_capacity(usize::from(cols) * usize::from(rows) * usize::from(gens) * 12);
    for g in 1..=gens {
        out.extend_from_slice(b"\x1b[H");
        for r in 1..=rows {
            for c in 1..=cols {
                let n = 16 + (u32::from(c) + u32::from(r) + u32::from(g)) % 216;
                write!(&mut out, "\x1b[38;5;{n}mX").expect("vec write");
            }
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
    }
    out.extend_from_slice(b"\x1b[0mCOLORDONE\r\n");
    out
}

/// Seed command for the heavy-colored burst: wait until `gate` exists,
/// `cat` precomputed `burst` bytes (from [`colored_burst_bytes`]), then
/// idle so the pane stays up for teardown.
///
/// The burst used to be a nested `/bin/sh` concat loop. Under CPU load
/// that loop took longer than the converge wait, so the gate failed
/// "colored burst never completed" without ever checking the ceiling
/// (phux-iuxr). `cat` of precomputed bytes is the same VT shape with an
/// emit cost that does not track host load. The gate file (same pattern
/// as the lagged-resync fixtures) starts the dump only after attach, so
/// a fast `cat` cannot land in the opening snapshot.
#[must_use]
pub fn colored_burst_command(burst: &Path, gate: &Path) -> CommandBuilder {
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; cat -- '{}'; sleep 30",
        gate.display(),
        burst.display(),
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

#[cfg(test)]
mod colored_burst_tests {
    use super::colored_burst_bytes;

    #[test]
    fn colored_burst_bytes_follow_the_cell_formula() {
        let bytes = colored_burst_bytes(3, 2, 2);
        let text = String::from_utf8(bytes).expect("ascii VT");
        assert!(text.contains("COLORDONE"));
        assert_eq!(text.matches("\u{1b}[H").count(), 2);
        // First cell of gen 1, row 1, col 1: n = 16 + (1+1+1)%216 = 19.
        assert!(text.contains("\u{1b}[38;5;19mX"));
        assert_eq!(colored_burst_bytes(3, 2, 2), colored_burst_bytes(3, 2, 2));
    }
}

#[cfg(test)]
mod client_oracle_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::Duration;

    use bytes::Bytes;
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::FrameKind;
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::UnixStream;

    use super::{
        BufferedFrameReceiver, ClientHandle, ReplicaGeneration, ScreenOracle, ViewportInfo,
    };
    use crate::{encode_frame, run_local};

    fn stream(raw: u64) -> StreamId {
        StreamId::new(raw).expect("non-zero stream id")
    }

    fn bootstrap(raw: u64) -> BootstrapId {
        BootstrapId::new(raw).expect("non-zero bootstrap id")
    }

    fn begin(terminal_id: &ResourceId, generation: u64, cols: u16, rows: u16) -> FrameKind {
        begin_at(terminal_id, generation, cols, rows, 0)
    }

    fn begin_at(
        terminal_id: &ResourceId,
        generation: u64,
        cols: u16,
        rows: u16,
        base_seq: u64,
    ) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(generation),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq,
        }
    }

    fn chunk(
        terminal_id: &ResourceId,
        generation: u64,
        chunk_seq: u32,
        payload: &'static [u8],
    ) -> FrameKind {
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(generation),
            chunk_seq,
            payload: Bytes::from_static(payload),
        }
    }

    fn ready(terminal_id: &ResourceId, generation: u64) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(generation),
            history_cursor: None,
        }
    }

    fn output(
        terminal_id: &ResourceId,
        generation: u64,
        seq: u64,
        bytes: &'static [u8],
    ) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: stream(1),
            bootstrap_id: bootstrap(generation),
            seq,
            bytes: Bytes::from_static(bytes),
        }
    }

    fn published_oracle(terminal_id: &ResourceId, generation: u64, base_seq: u64) -> ScreenOracle {
        let mut oracle = ScreenOracle::new(terminal_id.clone());
        let _ = oracle.apply(&begin_at(terminal_id, generation, 20, 2, base_seq));
        let _ = oracle.apply(&chunk(terminal_id, generation, 0, b"BASE"));
        let _ = oracle.apply(&ready(terminal_id, generation));
        oracle
    }

    fn assert_panics(f: impl FnOnce()) {
        assert!(catch_unwind(AssertUnwindSafe(f)).is_err());
    }

    #[test]
    fn matching_bootstrap_chunk_sequence_violations_panic() {
        let terminal_id = ResourceId::local(7);

        let mut duplicate = ScreenOracle::new(terminal_id.clone());
        let _ = duplicate.apply(&begin(&terminal_id, 1, 20, 2));
        let _ = duplicate.apply(&chunk(&terminal_id, 1, 0, b"FIRST"));
        assert_panics(|| {
            let _ = duplicate.apply(&chunk(&terminal_id, 1, 0, b"DUPLICATE"));
        });

        let mut gap = ScreenOracle::new(terminal_id.clone());
        let _ = gap.apply(&begin(&terminal_id, 2, 20, 2));
        assert_panics(|| {
            let _ = gap.apply(&chunk(&terminal_id, 2, 1, b"GAP"));
        });

        let mut exhausted = ScreenOracle::new(terminal_id.clone());
        let _ = exhausted.apply(&begin(&terminal_id, 3, 20, 2));
        exhausted
            .staging
            .as_mut()
            .expect("staging generation")
            .next_chunk_seq = Some(u32::MAX);
        let _ = exhausted.apply(&chunk(&terminal_id, 3, u32::MAX, b"LAST"));
        let staging = exhausted.staging.as_mut().expect("staging generation");
        assert!(staging.replica.screen.contains("LAST"));
        assert_eq!(staging.next_chunk_seq, None);
        assert_panics(|| {
            let _ = exhausted.apply(&chunk(&terminal_id, 3, u32::MAX, b"AFTER_LAST"));
        });
    }

    #[test]
    fn matching_live_output_sequence_violations_panic_but_mismatches_are_ignored() {
        let terminal_id = ResourceId::local(7);
        let wrong_terminal = ResourceId::local(99);

        let mut duplicate = published_oracle(&terminal_id, 1, 40);
        let _ = duplicate.apply(&output(&terminal_id, 1, 41, b"FIRST"));
        assert_panics(|| {
            let _ = duplicate.apply(&output(&terminal_id, 1, 41, b"DUPLICATE"));
        });

        let mut gap = published_oracle(&terminal_id, 2, 40);
        assert_panics(|| {
            let _ = gap.apply(&output(&terminal_id, 2, 42, b"GAP"));
        });

        let mut mismatched = published_oracle(&terminal_id, 3, 40);
        assert_eq!(
            mismatched.apply(&output(&terminal_id, 99, 41, b"STALE")),
            super::AppliedFrame::Ignored
        );
        assert_eq!(
            mismatched.apply(&output(&wrong_terminal, 3, 41, b"WRONG_OWNER")),
            super::AppliedFrame::Ignored
        );
        let screen = mismatched.screen_mut();
        assert!(!screen.contains("STALE"));
        assert!(!screen.contains("WRONG_OWNER"));
    }

    #[test]
    fn expired_receive_deadline_preserves_buffered_frame() {
        run_local(async {
            let (mut stream, _peer) = UnixStream::pair().expect("loopback pair");
            let mut receiver = BufferedFrameReceiver::default();
            receiver
                .buffered
                .extend_from_slice(&encode_frame(&FrameKind::Detach));
            let buffered_len = receiver.buffered.len();

            assert!(matches!(
                receiver
                    .receive_before(&mut stream, tokio::time::Instant::now())
                    .await,
                super::ReceiveBefore::Deadline
            ));
            assert_eq!(receiver.buffered.len(), buffered_len);
            assert!(matches!(
                receiver
                    .receive_before(
                        &mut stream,
                        tokio::time::Instant::now() + Duration::from_secs(1)
                    )
                    .await,
                super::ReceiveBefore::Frame(FrameKind::Detach)
            ));
            assert!(receiver.buffered.is_empty());
        });
    }

    #[test]
    fn replacement_bootstrap_survives_partial_read_and_ignores_stale_frames() {
        run_local(async {
            let terminal_id = ResourceId::local(7);
            let mut oracle = ScreenOracle::new(terminal_id.clone());
            let _ = oracle.apply(&begin(&terminal_id, 1, 20, 2));
            let _ = oracle.apply(&chunk(&terminal_id, 1, 0, b"OLD"));
            let _ = oracle.apply(&ready(&terminal_id, 1));

            let (client_stream, mut peer) = UnixStream::pair().expect("loopback pair");
            let mut client = ClientHandle {
                stream: client_stream,
                receiver: BufferedFrameReceiver::default(),
                oracle,
                terminal_id: terminal_id.clone(),
                client_id: 1,
                session: "test".to_owned(),
                viewport: ViewportInfo::new(20, 2),
                socket_path: std::path::PathBuf::new(),
            };

            let replacement = encode_frame(&begin(&terminal_id, 2, 24, 3));
            peer.write_all(&replacement[..6])
                .await
                .expect("write partial frame");
            let first_wait = client
                .wait_until_with_timeout(Duration::from_millis(10), |screen| {
                    screen.contains("COLORDONE")
                })
                .await;
            assert!(
                first_wait.is_err(),
                "partial frame must respect caller budget"
            );
            assert!(
                client.snapshot_text().contains("OLD"),
                "an incomplete replacement must not disturb the published screen"
            );

            let wrong_terminal = ResourceId::local(99);
            let trailing = [
                chunk(&terminal_id, 1, 1, b"STALE"),
                chunk(&wrong_terminal, 2, 0, b"WRONG_OWNER"),
                chunk(&terminal_id, 2, 0, b"\x1b[2J\x1b[HCOLORDONE"),
                ready(&terminal_id, 1),
                ready(&terminal_id, 2),
            ];
            let mut wire = replacement[6..].to_vec();
            for frame in &trailing {
                wire.extend_from_slice(&encode_frame(frame));
            }
            peer.write_all(&wire)
                .await
                .expect("write replacement frames");

            client
                .wait_until_with_timeout(Duration::from_secs(1), |screen| {
                    screen.contains("COLORDONE")
                })
                .await
                .expect("matching replacement must publish");
            let screen = client.screenshot().await;
            assert!(screen.contains("COLORDONE"));
            assert!(!screen.contains("STALE"));
            assert!(!screen.contains("WRONG_OWNER"));
            assert_eq!(
                screen.rows().len(),
                3,
                "BEGIN geometry must own replacement"
            );
            assert_eq!(
                client
                    .oracle
                    .published
                    .as_ref()
                    .map(|published| &published.generation),
                Some(&ReplicaGeneration::new(
                    &terminal_id,
                    stream(1),
                    bootstrap(2)
                ))
            );
        });
    }
}
