//! `E2eBuilder`: spin a server with a PTY seed pane, attach N clients, and
//! drive them through [`ClientHandle`], which owns the wire stream and a
//! replica-aware [`Screen`] oracle fed by every drained render frame. Drive
//! it from inside [`crate::run_local`] (the oracle and server are `!Send`).

#![allow(
    clippy::future_not_send,
    reason = "harness futures run on a LocalSet; the inner Screen + server are !Send"
)]
#![allow(clippy::assigning_clones, reason = "tests: clarity over micro-opt")]

use std::collections::HashSet;
use std::future::Future;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    FrameKind, TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    ViewportInfo,
};
use phux_protocol::{ResourceId, wire::frame::MAX_FRAME_LEN};
use phux_server::LaunchCommand;
use tokio::io::AsyncReadExt as _;
use tokio::net::UnixStream;
use tokio::time::timeout_at;

use super::screen::Screen;
use super::{
    SOCKET_CONNECT_DEADLINE, SpawnedServer, WIRE_RECV_TIMEOUT, recv_typed, send_frame,
    wait_for_socket,
};

/// Default oracle viewport. Matches [`crate::attach_by_name`] (80x24) so the
/// `Screen` dimensions line up with the `ATTACH` the harness sends.
const DEFAULT_COLS: u16 = 80;
/// See [`DEFAULT_COLS`].
const DEFAULT_ROWS: u16 = 24;

/// Silence after the last render that [`ClientHandle::converge`] treats as
/// settled: a few 33 Hz broadcast ticks.
pub const DEFAULT_IDLE_MS: u64 = 150;

/// Chainable spin-up for an e2e scenario: server, socket wait, and N
/// attached clients in one `run(|clients| async { ... })` call.
///
/// ```ignore
/// E2eBuilder::new()
///     .session("default")
///     .seed_cmd(LaunchCommand::new("/bin/cat"))
///     .clients(2)
///     .run(|mut clients| async move {
///         clients[0].send_text("hi\r").await;
///         clients[1].wait_until(|s| s.contains("hi")).await;
///     });
/// ```
pub struct E2eBuilder {
    session: String,
    seed_cmd: Option<LaunchCommand>,
    clients: usize,
    viewport: ViewportInfo,
}

impl Default for E2eBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl E2eBuilder {
    /// Session `default`, a plain shell seed pane, one client, 80x24.
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

    /// Run `cmd` in the seed pane's PTY instead of the default shell.
    #[must_use]
    pub fn seed_cmd(mut self, cmd: LaunchCommand) -> Self {
        self.seed_cmd = Some(cmd);
        self
    }

    /// Number of clients attached before [`Self::run`]'s closure runs.
    #[must_use]
    pub fn clients(mut self, n: usize) -> Self {
        self.clients = n.max(1);
        self
    }

    /// Override the attach viewport and oracle dimensions.
    #[must_use]
    pub const fn viewport(mut self, cols: u16, rows: u16) -> Self {
        self.viewport = ViewportInfo::new(cols, rows);
        self
    }

    /// Spin the server, attach the clients, run `body`, then shut down and
    /// assert the socket was unlinked.
    pub async fn run<F, Fut>(self, body: F)
    where
        F: FnOnce(Vec<ClientHandle>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut harness = self.spawn().await;
        body(std::mem::take(&mut harness.clients)).await;
        harness.shutdown().await;
    }

    /// Spin the server and attach clients without a closure, for tests that
    /// manage teardown or add clients mid-scenario.
    pub async fn spawn(self) -> Harness {
        let cmd = self
            .seed_cmd
            .unwrap_or_else(|| LaunchCommand::new("/bin/sh"));
        let server = SpawnedServer::builder()
            .session(&self.session)
            .seed(cmd)
            .start();

        let mut clients = Vec::with_capacity(self.clients);
        for _ in 0..self.clients {
            clients.push(
                ClientHandle::attach(&server.socket, &self.session, self.viewport)
                    .await
                    .expect("client attach"),
            );
        }

        Harness { clients, server }
    }
}

/// The attached clients plus the handles that tear the server down.
pub struct Harness {
    /// One handle per attached client, in attach order.
    pub clients: Vec<ClientHandle>,
    /// The server, its socket, and the temp dir that holds it.
    pub server: SpawnedServer,
}

impl Harness {
    /// Attach an additional client to the same session mid-scenario.
    pub async fn attach_client(&self, viewport: ViewportInfo) -> ClientHandle {
        ClientHandle::attach(&self.server.socket, &self.session_name(), viewport)
            .await
            .expect("attach additional client")
    }

    /// The session the first client attached to.
    fn session_name(&self) -> String {
        self.clients
            .first()
            .map_or_else(|| "default".to_owned(), |c| c.session.clone())
    }

    /// Drop the clients, shut the server down, and assert the socket was
    /// unlinked.
    pub async fn shutdown(self) {
        drop(self.clients);
        self.server.stop().await;
    }
}

/// One attached client: the wire stream, its screen oracle, and the
/// focused pane's id.
pub struct ClientHandle {
    stream: UnixStream,
    receiver: BufferedFrameReceiver,
    oracle: ScreenOracle,
    /// The focused pane from the opening `ATTACHED`; input targets it.
    pub terminal_id: ResourceId,
    /// Server-allocated client id for this attachment.
    pub client_id: u32,
    session: String,
    viewport: ViewportInfo,
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
    /// Connect, attach to `session`, and drain the opening bootstrap.
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

        let oracle = receive_opening_oracle(&mut stream, &terminal_id).await?;

        Ok(Self {
            stream,
            receiver: BufferedFrameReceiver::default(),
            oracle,
            terminal_id,
            client_id,
            session: session.to_owned(),
            viewport,
        })
    }

    /// Send `text` as one trusted `INPUT_PASTE`.
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

    /// Drain the render frames already on the wire (until a 20ms quiet
    /// gap) and return the oracle.
    pub async fn screenshot(&mut self) -> &mut Screen {
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

    /// The oracle's text without draining the wire, safe against a seed
    /// that never goes quiet (where [`Self::screenshot`] never returns).
    pub fn snapshot_text(&mut self) -> String {
        self.oracle.screen_mut().snapshot_text()
    }

    /// Drain at most `max_frames` (stopping at a 5ms quiet gap). Call it
    /// inside a storm so a client that only sends cannot fill the server's
    /// outbound mailbox and wedge the shared current-thread runtime.
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

    /// Drain render frames until `pred` holds or [`WIRE_RECV_TIMEOUT`]
    /// elapses.
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

    /// [`wait_until`](Self::wait_until) with a caller budget, only for a
    /// drain that legitimately outlasts 15s on a small runner (phux-fheq).
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
            // A slow drain can race the server's self-exit; a clean EOF
            // reports the screen we have (phux-fheq).
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

    /// Drain until nothing renders for `idle_ms` after the first payload
    /// (the idle rule does not start before output does), or until
    /// [`WIRE_RECV_TIMEOUT`]. Returns first-byte-to-settle time.
    pub async fn converge(&mut self, idle_ms: u64) -> Duration {
        self.converge_until(idle_ms, |_| true).await
    }

    /// [`converge`](Self::converge), but a quiet gap is not settle until
    /// `pred` holds: a burst can pause past the idle window (phux-4s38).
    async fn converge_until<P>(&mut self, idle_ms: u64, pred: P) -> Duration
    where
        P: FnMut(&mut Screen) -> bool,
    {
        self.converge_until_with_timeout(idle_ms, WIRE_RECV_TIMEOUT, pred)
            .await
    }

    /// `converge_until` with a caller budget (the colored-output gate's
    /// 30s ceiling, phux-iuxr).
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
            // Only a complete screen waits out just the idle window.
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
        first_byte_at.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Send `VIEWPORT_RESIZE` (zeros included) and rebuild an empty oracle
    /// at that size, clamped to 1x1.
    pub async fn resize(&mut self, cols: u16, rows: u16) {
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

    /// Detach by dropping the wire stream (a hard client departure).
    pub fn detach(self) {
        drop(self.stream);
    }

    async fn receive_before(&mut self, deadline: tokio::time::Instant) -> ReceiveBefore {
        self.receiver
            .receive_before(&mut self.stream, deadline)
            .await
    }
}

/// The perf gate's colored burst: `gens` in-place repaints of the grid,
/// every cell its own 256-color SGR, then `COLORDONE`. Deterministic.
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

/// Seed command that `cat`s precomputed `burst` bytes once `gate` exists
/// (after attach, so it misses the opening snapshot), then idles. A shell
/// loop generating it under load outlasted the wait (phux-iuxr).
#[must_use]
pub fn colored_burst_command(burst: &Path, gate: &Path) -> LaunchCommand {
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.02; done; cat -- '{}'; sleep 30",
        gate.display(),
        burst.display(),
    );
    let mut cmd = LaunchCommand::new("/bin/sh");
    cmd.args(["-c", &script]);
    cmd
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

    use super::{BufferedFrameReceiver, ClientHandle, ScreenOracle, ViewportInfo};
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
        });
    }
}
