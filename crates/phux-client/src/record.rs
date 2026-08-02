//! Headless terminal capture — the observer half of session recording
//! (ADR-0060).
//!
//! Subscribes to one Terminal's byte stream with `ATTACH_TERMINAL`
//! (`docs/spec/L1.md` §5.1) and projects the frames the server pushes back
//! into asciicast events. Like [`crate::snapshot`] and [`crate::watch`],
//! this path is *side-effect-free*: it neither attaches the session nor
//! resizes the pane, so it is safe to run against a live session a human is
//! using.
//!
//! # The prohibition this module exists to honour
//!
//! This path MUST NEVER send [`FrameKind::Attach`] or
//! [`FrameKind::ViewportResize`]. A session-scoped `ATTACH` runs the
//! server's `apply_attach_viewport`, which drives `TIOCSWINSZ` on every pane
//! under the default `WindowSize::Smallest` policy, and the client's
//! `send_attach` hardcodes `current_viewport()` — which reports 80x24 when
//! there is no TTY, which is exactly the situation a headless recorder is
//! in. A recorder that attached would therefore silently shrink the user's
//! live session to 80x24. `ATTACH_TERMINAL` is specified (L1 §5.1) as a
//! non-resizing observer subscription: it registers the caller as an output
//! subscriber, primes it through negotiated `BOOTSTRAP_BEGIN/CHUNK/READY`,
//! then streams `TERMINAL_OUTPUT` deltas.
//! `NEVER_SENDS_ATTACH_OR_VIEWPORT_RESIZE` test below is the regression
//! guard.
//!
//! # Recorder HELLO profile
//!
//! Construction negotiates before any stateful frame, using the recorder
//! consumer shape `phux_protocol::caps` already anticipates: the default
//! L1-only `LayerSet`, the default `OutputMode::Raw`, and — load-bearing —
//! [`ColorSupport::TrueColor`], which makes the server's per-client
//! `downsample::rewrite_bytes` a no-op so the captured bytes are the PTY's
//! verbatim.
//!
//! # Two accepted fidelity limits
//!
//! 1. **Timing resolution is the server's output pacing.** The server
//!    coalesces PTY bytes at its pacing rate (60 Hz by default, RTT-adaptive
//!    — `docs/spec/proto.md` §8), so sub-16 ms keystroke cadence is not
//!    recoverable and a `yes` flood replays chunkier than it looked live.
//!    There is no per-byte timestamp on the wire and adding one would be a
//!    wire change.
//! 2. **A mid-session recording opens on the viewport, not the history.**
//!    The server's `ATTACH_TERMINAL` handler requests a snapshot with
//!    `scrollback: None`, so the first frame of the recording is the current
//!    grid; whatever scrolled off above it is not in the capture.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use phux_protocol::caps::{BootstrapStreamProfile, ClientCapabilities, ColorSupport};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, FrameKind};
use phux_protocol::{BootstrapId, StreamId};
use phux_record::cast::{CastEvent, EventCode};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Request id carried by the `ATTACH_TERMINAL` command.
const REQUEST_ATTACH: u32 = 1;
/// Request id carried by the best-effort `DETACH_TERMINAL` teardown.
const REQUEST_DETACH: u32 = 2;
/// Floor on the interval between two `progress` callbacks. A recorder can
/// receive thousands of output frames a second; the CLI's spinner does not
/// need to repaint at that rate.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Data written for an `x` (exit) event when the pane died from a signal or
/// the server could not determine a status. `-1` keeps the field numeric for
/// anything that parses it, and no real `_exit(n)` can produce it.
const UNKNOWN_EXIT_STATUS: &str = "-1";

/// A finished headless capture, ready to be serialized as an asciicast.
///
/// `cols`/`rows` come from the first `BOOTSTRAP_BEGIN`, and `events` use
/// `phux-record`'s absolute-millisecond timebase with `t = 0` at bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessRecording {
    /// Grid width the recording must be replayed at.
    pub cols: u16,
    /// Grid height the recording must be replayed at.
    pub rows: u16,
    /// Wall-clock start of the capture, as Unix seconds — the asciicast
    /// header's `timestamp` key.
    pub started_unix: u64,
    /// The captured events, in arrival order.
    pub events: Vec<CastEvent>,
}

/// Record `terminal_id` as a pure observer until the pane exits, the server
/// goes away, `max_duration` elapses, or the caller hits Ctrl-C.
///
/// Opens its own connection, handshakes, subscribes with `ATTACH_TERMINAL` +
/// `SUBSCRIBE_EVENTS`, and drains the resulting stream. Every stop condition
/// except a transport or protocol failure is a *success*: Ctrl-C, the
/// duration cap, a clean server EOF, and the pane closing all return what
/// was captured so far, because a truncated recording is still playable.
///
/// `progress` is invoked with the elapsed capture time and the running event
/// count, at most once per 250 ms, so a CLI can render a live counter
/// without the callback becoming the hot path.
///
/// # Errors
///
/// Returns [`AttachError`] when the connect, the handshake, or the
/// subscription fails, when the server pushes an `ERROR` frame mid-stream
/// ([`AttachError::Refused`]), or on a transport failure that is not a clean
/// EOF.
pub async fn record_terminal(
    socket: &Path,
    terminal_id: TerminalId,
    max_duration: Option<Duration>,
    progress: impl FnMut(Duration, usize),
) -> Result<HeadlessRecording, AttachError> {
    let mut conn =
        Connection::connect_with_hello(socket, recorder_client_name(), recorder_client_caps())
            .await?;
    let outcome = record_on_connection(&mut conn, terminal_id, max_duration, progress).await;
    conn.shutdown().await;
    outcome
}

/// [`record_terminal`] minus the connect and the teardown, so the unit tests
/// can drive the whole sequence over a `UnixStream::pair`.
async fn record_on_connection(
    conn: &mut Connection,
    terminal_id: TerminalId,
    max_duration: Option<Duration>,
    progress: impl FnMut(Duration, usize),
) -> Result<HeadlessRecording, AttachError> {
    // A failure before the subscription exists has nothing to tear down, so
    // `?` here (rather than after the pump) is deliberate: it keeps the
    // best-effort DETACH_TERMINAL below paired with an actual subscription.
    let primed = subscribe(conn, &terminal_id).await?;
    let outcome = pump(conn, primed, max_duration, progress).await;
    // Best-effort teardown. DETACH_TERMINAL is idempotent and a no-op when
    // the Terminal is already gone (L1 §5.1), so it can never turn a
    // successful capture into a failure — and a send onto a socket the
    // server already closed is exactly as uninteresting.
    let _ = conn
        .send(&FrameKind::Command {
            request_id: REQUEST_DETACH,
            command: Command::DetachTerminal { terminal_id },
        })
        .await;
    outcome
}

/// Subscribe to the Terminal's content and event streams.
///
/// The connection has already negotiated the recorder's custom profile. The
/// stateful frame order asserted by the tests is therefore `HELLO` ->
/// `COMMAND { AttachTerminal }` -> `SUBSCRIBE_EVENTS`.
///
/// Returns the frames that arrived interleaved with the `COMMAND_RESULT` --
/// in practice the priming `TERMINAL_SNAPSHOT`. They are already consumed off
/// the socket, so the caller must feed them to the capture; nothing re-sends
/// them.
async fn subscribe(
    conn: &mut Connection,
    terminal_id: &TerminalId,
) -> Result<Vec<FrameKind>, AttachError> {
    // SPEC §5 permits the priming TERMINAL_SNAPSHOT to arrive before the
    // COMMAND_RESULT, and the reference server always sends it that way
    // (crates/phux-server/src/runtime/commands.rs: the snapshot is pushed
    // "before the pump's first delta and before the Ok reply"). So the
    // interleave is the normal case, not an edge case -- but a frame
    // observed here is CONSUMED, and the snapshot is never re-sent. Dropping
    // it silently is what made an entire capture come back as a 0x0 grid
    // with nothing playable in it. `Connection::request` hands every non-reply
    // frame back, so the pump can absorb them in arrival order; an ERROR
    // among them reaches `Capture::absorb`, which fails the capture.
    let (result, primed) = conn
        .request(
            REQUEST_ATTACH,
            Command::AttachTerminal {
                terminal_id: terminal_id.clone(),
            },
        )
        .await?
        .into_parts();
    if let CommandResult::Error { message, .. } = result {
        return Err(AttachError::Refused(message));
    }

    conn.send(&FrameKind::SubscribeEvents {
        terminal: Some(terminal_id.clone()),
    })
    .await?;
    Ok(primed)
}

fn recorder_client_name() -> String {
    format!("phux-rec/{}", env!("CARGO_PKG_VERSION"))
}

fn recorder_client_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_color_support(ColorSupport::TrueColor)
}

/// Drain the subscription into asciicast events until a stop condition.
async fn pump(
    conn: &mut Connection,
    primed: Vec<FrameKind>,
    max_duration: Option<Duration>,
    mut progress: impl FnMut(Duration, usize),
) -> Result<HeadlessRecording, AttachError> {
    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let start = Instant::now();
    let mut state = Capture::new(started_unix);

    // Frames the handshake consumed while waiting for its COMMAND_RESULT --
    // normally the priming snapshot. They predate the timer, so they land at
    // t=0, which is exactly where a cast's opening screen belongs.
    for frame in primed {
        if state.absorb(frame, Duration::ZERO)? {
            return Ok(state.finish());
        }
    }

    // Both stop arms are built once and polled by reference: a fresh
    // `sleep`/`ctrl_c` future per loop iteration would restart the timer on
    // every frame and re-register the signal handler thousands of times.
    let deadline = async move {
        match max_duration {
            Some(limit) => tokio::time::sleep(limit).await,
            // No cap: never resolve, so the select! arm is inert.
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);

    // First tick, fired from the priming snapshot rather than from the first
    // frame the loop below receives. Two reasons, both of which only became
    // visible once the unit tests started running against the reference
    // server's ordering (phux-h5hj.3): the opening screen is already an event
    // by the time the loop starts, and a pane that exits immediately delivers
    // `PANE_CLOSED` as the loop's *first* frame — which `break`s before ever
    // reaching the tick at the bottom. Ticking only inside the loop therefore
    // left a short capture with no counter update at all, which is exactly
    // the 250 ms dead zone this callback exists to avoid.
    let mut last_progress: Option<Instant> = Some(Instant::now());
    progress(start.elapsed(), state.events.len());
    loop {
        let frame = tokio::select! {
            () = &mut deadline => break,
            // Ctrl-C is a normal, successful stop — the same contract
            // `phux watch` offers. A failure to install the handler is not
            // worth failing a capture over, so it stops the loop too.
            _ = &mut interrupt => break,
            frame = conn.recv() => frame,
        };
        let elapsed = start.elapsed();
        match frame {
            Ok(frame) => {
                if state.absorb(frame, elapsed)? {
                    break;
                }
            }
            // A clean EOF means the server exited or the session ended.
            // Finalize what we have; a truncated recording is playable.
            Err(AttachError::Disconnected) => break,
            Err(err) => return Err(err),
        }
        if last_progress.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
            last_progress = Some(Instant::now());
            progress(elapsed, state.events.len());
        }
    }

    Ok(state.finish())
}

/// The event-accumulating half of [`pump`], split out so the frame-to-event
/// projection is testable and readable without the `select!` machinery.
struct Capture {
    started_unix: u64,
    /// The dimensions of the priming snapshot — the geometry a replay must
    /// start at, and therefore the cast header's. Later resizes live in the
    /// timeline as `r` events, not here.
    opening: Option<(u16, u16)>,
    /// `None` until the priming `TERMINAL_SNAPSHOT` lands; afterwards the
    /// last dimensions the server reported, which is what distinguishes a
    /// genuine resize from a lag-recovery resync.
    dims: Option<(u16, u16)>,
    events: Vec<CastEvent>,
    /// Bytes held back because they are an incomplete UTF-8 sequence.
    tail: Vec<u8>,
    /// Active synthesized bootstrap and next required chunk sequence.
    bootstrap: Option<(TerminalId, StreamId, BootstrapId, u32)>,
}

impl Capture {
    const fn new(started_unix: u64) -> Self {
        Self {
            started_unix,
            opening: None,
            dims: None,
            events: Vec::new(),
            tail: Vec::new(),
            bootstrap: None,
        }
    }

    /// Project one server frame into zero or more events.
    ///
    /// Returns `Ok(true)` when the frame ends the recording.
    fn absorb(&mut self, frame: FrameKind, elapsed: Duration) -> Result<bool, AttachError> {
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols,
                rows,
                ..
            } => {
                self.snapshot(cols, rows, &[], elapsed);
                self.bootstrap = Some((terminal_id, stream_id, bootstrap_id, 0));
                Ok(false)
            }
            FrameKind::BootstrapBegin { profile, .. } => Err(AttachError::Protocol(format!(
                "recorder requires synthesized raw VT bootstrap, got {profile:?}"
            ))),
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                let Some((expected_terminal, expected_stream, expected_bootstrap, next_chunk)) =
                    self.bootstrap.as_mut()
                else {
                    return Err(AttachError::Protocol(
                        "bootstrap chunk arrived before begin".to_owned(),
                    ));
                };
                if terminal_id != *expected_terminal
                    || stream_id != *expected_stream
                    || bootstrap_id != *expected_bootstrap
                    || chunk_seq != *next_chunk
                {
                    return Err(AttachError::Protocol(
                        "bootstrap chunk generation or sequence mismatch".to_owned(),
                    ));
                }
                *next_chunk = next_chunk.saturating_add(1);
                self.output(&payload, elapsed);
                Ok(false)
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => {
                let Some((expected_terminal, expected_stream, expected_bootstrap, _)) =
                    self.bootstrap.take()
                else {
                    return Err(AttachError::Protocol(
                        "bootstrap ready arrived before begin".to_owned(),
                    ));
                };
                if terminal_id != expected_terminal
                    || stream_id != expected_stream
                    || bootstrap_id != expected_bootstrap
                {
                    return Err(AttachError::Protocol(
                        "bootstrap ready generation mismatch".to_owned(),
                    ));
                }
                Ok(false)
            }
            // `seq` is the pump's per-consumer sequence id, not a timebase;
            // arrival order is the only ordering this projection needs.
            FrameKind::TerminalOutput { bytes, .. } => {
                self.output(&bytes, elapsed);
                Ok(false)
            }
            FrameKind::Event {
                event: AgentEvent::PaneClosed { exit_status },
                ..
            } => {
                self.flush_tail(elapsed);
                let data = exit_status.map_or_else(
                    || UNKNOWN_EXIT_STATUS.to_owned(),
                    |status| status.to_string(),
                );
                self.push(elapsed, EventCode::Exit, data);
                Ok(true)
            }
            FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
            // Anything else the server interleaves (BELL, PONG, other agent
            // events) is not part of the byte stream a cast replays.
            _other => Ok(false),
        }
    }

    /// Absorb a `TERMINAL_SNAPSHOT`.
    ///
    /// The first one seeds the header dimensions and lands at `t = 0` — the
    /// recording opens on the grid as it was, not on a blank screen. A later
    /// one is either a genuine resize or the output pump's lag-recovery
    /// resync, and the *only* way to tell them apart is to compare the
    /// dimensions: emitting a spurious `r` would make the renderer rebuild
    /// its canvas for nothing. Either way the replay bytes are a full-grid
    /// repaint, so they always follow as an `o`.
    fn snapshot(&mut self, cols: u16, rows: u16, replay: &[u8], elapsed: Duration) {
        let at = match self.dims {
            None => {
                self.opening = Some((cols, rows));
                self.dims = Some((cols, rows));
                Duration::ZERO
            }
            Some(previous) => {
                if previous != (cols, rows) {
                    self.dims = Some((cols, rows));
                    // Order matters: the resize has to be in the timeline
                    // before the bytes that assume the new geometry.
                    self.flush_tail(elapsed);
                    self.push(elapsed, EventCode::Resize, format!("{cols}x{rows}"));
                }
                elapsed
            }
        };
        self.output(replay, at);
    }

    /// Absorb terminal bytes, decoding as much valid UTF-8 as is available.
    fn output(&mut self, bytes: &[u8], elapsed: Duration) {
        let text = self.decode(bytes);
        if text.is_empty() {
            return;
        }
        self.push(elapsed, EventCode::Output, text);
    }

    /// Emit any residual carry as U+FFFD before a non-output event.
    ///
    /// A tail still pending when the stream ends (or when a resize/exit
    /// splits it) is genuinely truncated, so it surfaces as a replacement
    /// character rather than vanishing — the same choice `CastWriter::finish`
    /// makes, for the same reason: a recording that swallows its last byte is
    /// harder to debug than one that shows the damage.
    fn flush_tail(&mut self, elapsed: Duration) {
        if !self.tail.is_empty() {
            self.tail.clear();
            self.push(elapsed, EventCode::Output, "\u{fffd}".to_owned());
        }
    }

    fn push(&mut self, at: Duration, code: EventCode, data: String) {
        let time_ms = u64::try_from(at.as_millis()).unwrap_or(u64::MAX);
        self.events.push(CastEvent {
            time_ms,
            code,
            data,
        });
    }

    /// Decode `bytes` against the pending carry, retaining an incomplete
    /// trailing sequence for the next frame.
    ///
    /// PTY output is a raw byte stream and a multi-byte character routinely
    /// straddles two `TERMINAL_OUTPUT` frames. `String::from_utf8_lossy` per
    /// frame would splice a U+FFFD into every box-drawing and emoji
    /// recording at the frame boundaries. This mirrors `CastWriter::output`'s
    /// carry in `phux-record`; the two must agree, because the same bytes may
    /// reach a `.cast` through either one.
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.tail.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.tail) {
                Ok(text) => {
                    out.push_str(text);
                    self.tail.clear();
                    return out;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.tail[..valid]).unwrap_or_default());
                    match err.error_len() {
                        // A prefix of a legal sequence: hold it for the next
                        // frame, which is the whole point of the carry.
                        None => {
                            self.tail.drain(..valid);
                            return out;
                        }
                        // Genuinely invalid: one U+FFFD, consume it, keep
                        // going — the rest of the chunk is still good data.
                        Some(bad) => {
                            out.push('\u{fffd}');
                            self.tail.drain(..valid.saturating_add(bad));
                        }
                    }
                }
            }
        }
    }

    /// Close the capture, flushing a truncated trailing sequence.
    fn finish(mut self) -> HeadlessRecording {
        let at = self
            .events
            .last()
            .map_or(Duration::ZERO, |last| Duration::from_millis(last.time_ms));
        self.flush_tail(at);
        // A capture that never saw a snapshot (the server hung up between
        // the COMMAND_RESULT and the priming frame) reports 0x0 rather than
        // inventing a geometry; the caller decides whether an empty
        // recording is worth writing.
        let (cols, rows) = self.opening.unwrap_or((0, 0));
        HeadlessRecording {
            cols,
            rows,
            started_unix: self.started_unix,
            events: self.events,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};
    use tokio::net::UnixStream;

    fn terminal() -> TerminalId {
        TerminalId::local(7)
    }

    fn snapshot(cols: u16, rows: u16, replay: &[u8]) -> Vec<FrameKind> {
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(2).expect("bootstrap");
        vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols,
                rows,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::copy_from_slice(replay),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ]
    }

    fn output(seq: u64, bytes: &[u8]) -> FrameKind {
        FrameKind::TerminalOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            seq,
            bytes: bytes::Bytes::copy_from_slice(bytes),
        }
    }

    fn pane_closed(exit_status: Option<i32>) -> FrameKind {
        FrameKind::Event {
            terminal: Some(terminal()),
            event: AgentEvent::PaneClosed { exit_status },
        }
    }

    /// A session whose client will attach a Terminal.
    ///
    /// The priming `TERMINAL_SNAPSHOT` is not optional and is not a "script"
    /// frame: [`crate::testkit`] sends it *before* the `ATTACH_TERMINAL` ack,
    /// because that is what `handle_attach_terminal` does. Every test below
    /// therefore exercises the interleave, not just the one guard that was
    /// written after the bug escaped.
    fn attached(cols: u16, rows: u16, replay: &[u8]) -> ScriptSpec {
        ScriptSpec::new().priming_snapshot(&terminal(), cols, rows, replay)
    }

    #[test]
    fn snapshot_arriving_before_the_command_result_is_not_dropped() {
        let (recorded, _seen) = block_on(run(
            attached(120, 40, b"opening screen")
                .push(output(1, b" more"))
                .push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");

        assert_eq!(
            (recorded.cols, recorded.rows),
            (120, 40),
            "the priming TERMINAL_SNAPSHOT arrives BEFORE the COMMAND_RESULT on \
             a real server. A handshake that consumes and discards it leaves the \
             capture with no geometry (0x0) and no opening screen, so the whole \
             recording is unplayable. Feed pre-ack frames to the capture."
        );
        assert_eq!(
            recorded.events.first().map(|ev| ev.data.as_str()),
            Some("opening screen"),
            "the opening screen must be the first event, at t=0"
        );
        assert_eq!(
            codes(&recorded),
            vec![EventCode::Output, EventCode::Output, EventCode::Exit],
        );
    }

    /// Run one scripted session and return both halves' results.
    async fn run(
        spec: ScriptSpec,
        max_duration: Option<Duration>,
    ) -> (Result<HeadlessRecording, AttachError>, Vec<FrameKind>) {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let server_side =
            tokio::task::spawn_local(ScriptedServer::on_stream(server_stream, spec).run());
        client
            .negotiate(recorder_client_name(), recorder_client_caps())
            .await
            .expect("recorder HELLO");
        let recorded = record_on_connection(&mut client, terminal(), max_duration, |_, _| {}).await;
        let seen = server_side.await.expect("server task");
        (recorded, seen)
    }

    /// `Connection` holds `!Send` transport halves, so the scripted server
    /// runs on a `LocalSet` rather than `tokio::spawn`.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, fut)
    }

    fn codes(recording: &HeadlessRecording) -> Vec<EventCode> {
        recording.events.iter().map(|ev| ev.code).collect()
    }

    #[test]
    fn sends_hello_then_attach_terminal_then_subscribe_events() {
        let (recorded, seen) = block_on(run(
            attached(80, 24, b"hi").push(pane_closed(Some(0))),
            None,
        ));
        recorded.expect("recording");
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "HELLO must be the first frame so the recorder goes through the \
             version gate and authorize_hello, got {:?}",
            seen.first()
        );
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: Command::AttachTerminal { .. },
                    ..
                })
            ),
            "ATTACH_TERMINAL must follow HELLO, got {:?}",
            seen.get(1)
        );
        assert!(
            matches!(seen.get(2), Some(FrameKind::SubscribeEvents { .. })),
            "SUBSCRIBE_EVENTS must follow ATTACH_TERMINAL — it is the only \
             way an ATTACH_TERMINAL-only observer learns the pane exited; \
             got {:?}",
            seen.get(2)
        );
    }

    #[test]
    fn hello_advertises_truecolor_and_the_default_l1_layerset() {
        let (_recorded, seen) =
            block_on(run(attached(80, 24, b"").push(pane_closed(Some(0))), None));
        let Some(FrameKind::Hello {
            client_caps,
            client_name,
            ..
        }) = seen.first()
        else {
            panic!("first frame is HELLO");
        };
        assert_eq!(
            client_caps.color_support,
            ColorSupport::TrueColor,
            "TrueColor is what makes the server's downsample::rewrite_bytes a \
             no-op, so the recorded bytes are the PTY's verbatim"
        );
        assert_eq!(
            client_caps.layers,
            phux_protocol::caps::LayerSet::new(),
            "a recorder is an L1-only consumer"
        );
        assert_eq!(
            client_caps.output_mode,
            phux_protocol::caps::OutputMode::Raw,
            "raw bytes are the recording; state-sync would hand us diffs"
        );
        assert!(
            client_name.starts_with("phux-rec/"),
            "client_name identifies the recorder, got {client_name:?}"
        );
    }

    #[test]
    #[allow(non_snake_case, reason = "the name is the warning")]
    fn NEVER_SENDS_ATTACH_OR_VIEWPORT_RESIZE() {
        // A full session: snapshot, output, a resize-shaped second snapshot,
        // more output, then the pane closing.
        let (recorded, seen) = block_on(run(
            attached(120, 34, b"first")
                .push(output(1, b"more")).extend(snapshot(100, 30, b"repaint"))
                .push(output(2, b"tail"))
                .push(pane_closed(Some(0))),
            None,
        ));
        recorded.expect("recording");
        for frame in &seen {
            assert!(
                !matches!(
                    frame,
                    FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
                ),
                "the headless recorder sent {frame:?}. It must never send \
                 ATTACH or VIEWPORT_RESIZE: a session-scoped ATTACH runs the \
                 server's apply_attach_viewport, which drives TIOCSWINSZ on \
                 every pane under the default WindowSize::Smallest policy, \
                 and a headless caller's current_viewport() reports 80x24 \
                 with no TTY. The consequence of this regression is that \
                 running `phux rec` SHRINKS THE LIVE SESSION A HUMAN IS \
                 USING to 80x24. Use Command::AttachTerminal, which L1 §5.1 \
                 specifies as a non-resizing observer subscription."
            );
        }
    }

    #[test]
    fn first_snapshot_seeds_dims_and_emits_replay_bytes_at_time_zero() {
        let (recorded, _seen) = block_on(run(
            attached(120, 34, b"opening grid").push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");
        assert_eq!((recorded.cols, recorded.rows), (120, 34));
        let first = recorded.events.first().expect("an opening event");
        assert_eq!(first.code, EventCode::Output);
        assert_eq!(first.data, "opening grid");
        assert_eq!(
            first.time_ms, 0,
            "the priming snapshot is the recording's t=0, not whenever it \
             happened to arrive"
        );
    }

    #[test]
    fn second_snapshot_with_new_dims_emits_r_then_o() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"a").extend(snapshot(100, 30, b"b"))
                .push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");
        assert_eq!(
            codes(&recorded),
            vec![
                EventCode::Output,
                EventCode::Resize,
                EventCode::Output,
                EventCode::Exit
            ]
        );
        assert_eq!(recorded.events[1].data, "100x30");
        assert_eq!(recorded.events[2].data, "b");
        assert_eq!(
            (recorded.cols, recorded.rows),
            (80, 24),
            "the header keeps the opening dims; the resize is in the timeline"
        );
    }

    #[test]
    fn second_snapshot_with_identical_dims_emits_o_only() {
        // This is the lag-recovery resync the output pump injects. It is a
        // repaint, not a resize.
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"a").extend(snapshot(80, 24, b"resync"))
                .push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");
        assert_eq!(
            codes(&recorded),
            vec![EventCode::Output, EventCode::Output, EventCode::Exit],
            "a resync snapshot must not fabricate an `r` event"
        );
        assert_eq!(recorded.events[1].data, "resync");
    }

    #[test]
    fn terminal_output_frames_become_o_events_in_arrival_order() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"")
                .push(output(9, b"one"))
                .push(output(4, b"two"))
                .push(output(7, b"three"))
                .push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");
        let data: Vec<&str> = recorded
            .events
            .iter()
            .filter(|ev| ev.code == EventCode::Output)
            .map(|ev| ev.data.as_str())
            .collect();
        assert_eq!(
            data,
            vec!["one", "two", "three"],
            "arrival order wins; `seq` is the pump's per-consumer counter, \
             not a timebase"
        );
    }

    #[test]
    fn pane_closed_event_emits_x_with_the_exit_status_and_stops() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"")
                .push(pane_closed(Some(42)))
                // Never delivered: the client stops on PaneClosed.
                .push(output(1, b"after the end")),
            None,
        ));
        let recorded = recorded.expect("recording");
        let last = recorded.events.last().expect("an exit event");
        assert_eq!(last.code, EventCode::Exit);
        assert_eq!(last.data, "42");
        assert!(
            !recorded.events.iter().any(|ev| ev.data == "after the end"),
            "the loop must stop at PaneClosed"
        );
    }

    #[test]
    fn pane_closed_without_a_status_records_the_unknown_sentinel() {
        let (recorded, _seen) = block_on(run(attached(80, 24, b"").push(pane_closed(None)), None));
        let recorded = recorded.expect("recording");
        let last = recorded.events.last().expect("an exit event");
        assert_eq!(last.code, EventCode::Exit);
        assert_eq!(last.data, UNKNOWN_EXIT_STATUS);
    }

    #[test]
    fn error_frame_becomes_attach_error_refused() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"").push(FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            }),
            None,
        ));
        match recorded {
            Err(AttachError::Refused(message)) => assert_eq!(message, "no such terminal"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn transport_eof_finalizes_successfully() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"grid")
                .push(output(1, b"bytes"))
                .end(EndOfScript::HangUp),
            None,
        ));
        let recorded = recorded.expect("an EOF is a clean stop, not a failure");
        assert_eq!(
            codes(&recorded),
            vec![EventCode::Output, EventCode::Output],
            "everything received before the EOF is kept"
        );
    }

    #[test]
    fn max_duration_stops_the_loop_and_returns_what_was_captured() {
        // The script leaves the connection open with nothing more to send,
        // so only the duration cap can end the capture.
        let (recorded, seen) = block_on(run(
            attached(80, 24, b"grid").push(output(1, b"bytes")),
            Some(Duration::from_millis(120)),
        ));
        let recorded = recorded.expect("the duration cap is a successful stop");
        assert_eq!(codes(&recorded), vec![EventCode::Output, EventCode::Output]);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::DetachTerminal { .. },
                    ..
                }
            )),
            "a capped capture still tears its subscription down"
        );
    }

    #[test]
    fn detach_terminal_is_sent_on_clean_exit() {
        let (recorded, seen) =
            block_on(run(attached(80, 24, b"").push(pane_closed(Some(0))), None));
        recorded.expect("recording");
        let last = seen.last().expect("at least one client frame");
        assert!(
            matches!(
                last,
                FrameKind::Command {
                    command: Command::DetachTerminal { .. },
                    ..
                }
            ),
            "DETACH_TERMINAL closes the subscription cleanly, got {last:?}"
        );
    }

    #[test]
    fn utf8_split_across_two_output_frames_stays_one_character() {
        // U+2500 BOX DRAWINGS LIGHT HORIZONTAL, cut mid-sequence. A
        // per-frame from_utf8_lossy would produce two U+FFFD here.
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"")
                .push(output(1, &[0xe2, 0x94]))
                .push(output(2, &[0x80]))
                .push(pane_closed(Some(0))),
            None,
        ));
        let recorded = recorded.expect("recording");
        let text: String = recorded
            .events
            .iter()
            .filter(|ev| ev.code == EventCode::Output)
            .map(|ev| ev.data.as_str())
            .collect();
        assert_eq!(text, "\u{2500}");
        assert!(
            !text.contains('\u{fffd}'),
            "the carry must not splice a replacement character at the frame \
             boundary"
        );
    }

    #[test]
    fn a_truncated_utf8_tail_surfaces_as_a_replacement_character() {
        let (recorded, _seen) = block_on(run(
            attached(80, 24, b"")
                .push(output(1, &[0xe2, 0x94]))
                .end(EndOfScript::HangUp),
            None,
        ));
        let recorded = recorded.expect("recording");
        let last = recorded.events.last().expect("a flushed tail");
        assert_eq!(last.data, "\u{fffd}");
    }

    #[test]
    fn progress_is_reported_at_least_once_for_a_captured_session() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        let calls = local.block_on(&rt, async {
            let (client_stream, server_stream) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            let spec = attached(80, 24, b"grid").push(pane_closed(Some(0)));
            let server_side =
                tokio::task::spawn_local(ScriptedServer::on_stream(server_stream, spec).run());
            client
                .negotiate(recorder_client_name(), recorder_client_caps())
                .await
                .expect("recorder HELLO");
            let mut calls: Vec<(Duration, usize)> = Vec::new();
            let recorded = record_on_connection(&mut client, terminal(), None, |at, count| {
                calls.push((at, count));
            })
            .await;
            recorded.expect("recording");
            server_side.await.expect("server task");
            calls
        });
        assert!(
            !calls.is_empty(),
            "the first progress tick fires immediately so a CLI spinner \
             appears without a 250 ms dead zone"
        );
        assert!(
            calls.iter().all(|(_, count)| *count > 0),
            "progress reports the running event count"
        );
    }
}
