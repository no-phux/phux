//! Headless terminal capture: the observer half of session recording
//! (ADR-0060).
//!
//! Subscribes to one Terminal with `ATTACH_RESOURCE` (`docs/spec/L1.md`
//! §5.1), a non-resizing observer subscription, and projects the pushed
//! frames into asciicast events. It must never send [`FrameKind::Attach`] or
//! [`FrameKind::ViewportResize`]: from a TTY-less recorder those would shrink
//! the live session to 80x24.
//!
//! The recorder negotiates [`ColorSupport::TrueColor`] so the server does not
//! downsample and the captured bytes are the PTY's verbatim. Timing
//! resolution is the server's output pacing, and a mid-session recording
//! opens on the viewport, not the scrollback.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use phux_protocol::caps::{BootstrapStreamProfile, ClientCapabilities, ColorSupport};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, FrameKind};
use phux_protocol::{BootstrapId, StreamId};
use phux_record::cast::{CastEvent, EventCode};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Request id carried by the `ATTACH_RESOURCE` command.
const REQUEST_ATTACH: u32 = 1;
/// Request id carried by the best-effort `DETACH_RESOURCE` teardown.
const REQUEST_DETACH: u32 = 2;
/// Floor on the interval between two `progress` callbacks.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// `x` event data when the exit status is unknown: numeric, and no real
/// `_exit(n)` produces it.
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
/// Every stop except a transport or protocol failure returns what was
/// captured: a truncated recording is still playable. `progress` gets the
/// elapsed time and event count, at most every 250 ms.
///
/// # Errors
///
/// Returns [`AttachError`] when the connect, the handshake, or the
/// subscription fails, when the server pushes an `ERROR` frame mid-stream
/// ([`AttachError::Refused`]), or on a transport failure that is not a clean
/// EOF.
#[allow(
    clippy::significant_drop_tightening,
    reason = "Connection::shutdown consumes the connection at its final use; an explicit drop afterward is impossible"
)]
pub async fn record_terminal(
    socket: &Path,
    terminal_id: ResourceId,
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

/// [`record_terminal`] minus the connect and teardown, for the unit tests.
async fn record_on_connection(
    conn: &mut Connection,
    terminal_id: ResourceId,
    max_duration: Option<Duration>,
    progress: impl FnMut(Duration, usize),
) -> Result<HeadlessRecording, AttachError> {
    // Before the subscription exists there is nothing to tear down.
    let primed = subscribe(conn, &terminal_id).await?;
    let outcome = pump(conn, primed, max_duration, progress).await;
    // Best-effort teardown: DETACH_RESOURCE is idempotent (L1 §5.1).
    let _ = conn
        .send(&FrameKind::Command {
            request_id: REQUEST_DETACH,
            command: Command::DetachResource {
                terminal_id: terminal_id.clone(),
            },
        })
        .await;
    conn.unbind_terminal(&terminal_id);
    outcome
}

/// Subscribe to the Terminal's content and event streams.
///
/// Returns the frames interleaved ahead of the `COMMAND_RESULT` (the priming
/// bootstrap, which is never re-sent), so the caller must capture them.
async fn subscribe(
    conn: &mut Connection,
    terminal_id: &ResourceId,
) -> Result<Vec<FrameKind>, AttachError> {
    let (result, primed) = conn
        .request(
            REQUEST_ATTACH,
            // A viewer records everything and can type nothing (ADR-0127).
            Command::attach_terminal(terminal_id.clone(), conn.observer_role_policy()),
        )
        .await?
        .into_parts();
    if let CommandResult::Error { message, .. } = result {
        return Err(AttachError::Refused(message));
    }

    conn.bind_terminal(terminal_id).await?;

    conn.send(&FrameKind::SubscribeEvents {
        terminal: Some(terminal_id.clone()),
        after_seq: None,
    })
    .await?;
    Ok(primed)
}

fn recorder_client_name() -> String {
    format!("phux-rec/{}", env!("CARGO_PKG_VERSION"))
}

const fn recorder_client_caps() -> ClientCapabilities {
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

    // The priming snapshot predates the timer: it lands at t=0.
    for frame in primed {
        if state.absorb(frame, Duration::ZERO)? {
            return Ok(state.finish());
        }
    }

    // Stop arms are built once: per-iteration futures would restart the
    // timer on every frame.
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

    // Tick once up front: a pane that exits at once breaks before the
    // in-loop tick.
    let mut last_progress: Option<Instant> = Some(Instant::now());
    progress(start.elapsed(), state.events.len());
    loop {
        let frame = tokio::select! {
            () = &mut deadline => break,
            // Ctrl-C (or a failed handler install) is a successful stop.
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
            // A clean EOF: the server or session ended.
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

/// The frame-to-event projection behind [`pump`].
struct Capture {
    started_unix: u64,
    /// The priming snapshot's dimensions: the cast header's geometry.
    opening: Option<(u16, u16)>,
    /// The last reported dimensions; distinguishes a resize from a resync.
    dims: Option<(u16, u16)>,
    events: Vec<CastEvent>,
    /// Bytes held back because they are an incomplete UTF-8 sequence.
    tail: Vec<u8>,
    /// Active synthesized bootstrap and next required chunk sequence.
    bootstrap: Option<(ResourceId, StreamId, BootstrapId, u32)>,
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
            FrameKind::ResourceOutput { bytes, .. } => {
                self.output(&bytes, elapsed);
                Ok(false)
            }
            FrameKind::Event {
                event: AgentEvent::ResourceClosed { exit_status },
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

    /// Absorb a bootstrap's geometry: the first seeds the header at `t = 0`;
    /// a later one emits `r` only if the dimensions changed (else it is a
    /// lag-recovery resync).
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
                    // The resize precedes the bytes that assume it.
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

    /// Emit any residual carry as U+FFFD before a non-output event: a
    /// truncated tail shows the damage rather than vanishing (as
    /// `CastWriter::finish` does).
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
    /// trailing sequence for the next frame (characters straddle frames).
    /// Mirrors `CastWriter::output`'s carry in `phux-record`.
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
                        // A prefix of a legal sequence: hold it.
                        None => {
                            self.tail.drain(..valid);
                            return out;
                        }
                        // Invalid: one U+FFFD, then keep going.
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
        // No snapshot seen: report 0x0 rather than invent a geometry.
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
mod tests;
