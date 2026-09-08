//! The `phux --rec` session tee: an asciicast written from the client's own
//! composited output stream (ADR-0060).
//!
//! # Why the `RenderSink` and not a per-pane output tap
//!
//! The driver threads exactly ONE `&mut W: RenderSink` through the whole
//! render path (see [`super::RenderSink`]) — panes tiled per the layout,
//! dividers, status bar, sidebar, overlays, cursor restore. Wrapping that one
//! seam captures byte-for-byte what the human's glass received, which is the
//! thing users mean by "record my session": it replays in a fresh libghostty
//! `Terminal` sized to the viewport with no compositor anywhere in the loop.
//! A per-pane `TERMINAL_OUTPUT` tap would record one pane's PTY bytes and
//! lose every piece of chrome.
//!
//! The tee also sits deliberately UPSTREAM of
//! `StdoutSink`'s 256 KiB backlog drop. When a
//! terminal backpressures, that sink discards the stale queue and asks the
//! driver for a fresh full repaint — so a backpressured terminal loses frames
//! on the glass but never in the recording, because the tee already saw the
//! bytes on the way past.
//!
//! # Known fidelity limit
//!
//! `defer_paint` / `overlay_active` (see `server_frame.rs`) suppress stdout
//! emission without suppressing the pane mirror's `vt_write`, and the driver
//! coalesces bursts of frames into one paint. Recorded *timing* is therefore
//! the paint cadence, not the true per-frame arrival cadence: a fast burst
//! shows up as one chunky event rather than N small ones. The content is
//! exact; only the sub-paint timing is lost.
//!
//! # Failure policy
//!
//! Nothing in here may kill the session or write to stderr. This path owns
//! the alt screen — `is_interactive_client` is true for `phux` / `phux
//! attach`, tracing is file-only there, and one stray stderr byte corrupts
//! the display. A recording write failure (full disk, unlinked file) latches
//! a single `tracing::warn!` and then goes permanently silent; the prefix
//! already on disk stays playable, which is the whole point of the streaming
//! asciicast format.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use phux_record::cast::{CastHeader, CastVersion, CastWriter};
use phux_record::error::RecordError;

use super::outcome::AttachError;

/// Bytes of trailing whitespace reserved after the header line so
/// [`SessionRecorder::finish`] can rewrite it in place with the `duration`
/// key. `,"duration":86400.000` is 22 bytes; 32 leaves room for a recording
/// far longer than anyone will sit through.
const HEADER_SLACK: usize = 32;

/// How the recorder learns the outer terminal's current size.
///
/// Boxed rather than a generic parameter because the recorder is held behind
/// an `Rc<RefCell<_>>` that the driver names without type arguments; a second
/// generic there would leak into the driver's signature for no benefit.
type SizeProbe = Box<dyn FnMut() -> Option<(u16, u16)>>;

/// Read the controlling TTY's size, or `None` when stdout is not a terminal.
///
/// This is the same `tcgetwinsize` call `current_viewport` makes in the
/// driver; going straight to the ioctl instead of plumbing SIGWINCH into the
/// recorder keeps the driver diff to a handful of lines, and one ioctl per
/// frame flush (~60/s) is free next to the write it accompanies.
fn tty_winsize() -> Option<(u16, u16)> {
    let stdout = io::stdout();
    let size = rustix::termios::tcgetwinsize(stdout.as_fd()).ok()?;
    (size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The environment asciinema conventionally records. Deliberately just these
/// two: a recording is a shareable artifact and a full `environ` dump is how
/// tokens end up in a gist.
fn recorded_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["TERM", "SHELL"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_owned(), value);
        }
    }
    env
}

/// Map a codec error onto the attach error vocabulary.
///
/// Recording failures are all "the output file went wrong" from the caller's
/// point of view, so they collapse onto [`AttachError::Io`] rather than
/// growing a variant that only one CLI flag can ever produce.
fn record_err(err: RecordError) -> AttachError {
    match err {
        RecordError::Io(inner) => AttachError::Io(inner),
        other => AttachError::Io(io::Error::other(other.to_string())),
    }
}

/// Writer shim that reserves rewrite slack at the end of the header line.
///
/// `CastWriter` streams: it writes the header before the first event, so the
/// `duration` key — knowable only at the end — cannot be written up front.
/// The header line therefore lands on disk padded with trailing spaces (JSON
/// ignores whitespace after the closing brace, and every asciicast reader
/// parses one line at a time), leaving [`SessionRecorder::finish`] exactly
/// enough room to seek back and overwrite line 1 in place. The alternative —
/// rewriting the entire file to splice in a dozen bytes — is O(recording
/// size) at session exit for no user-visible gain.
struct SlackPad<W: Write> {
    inner: W,
    /// Spaces still owed before the header line's newline. Zero once the
    /// header is on disk, after which this type is a pure pass-through.
    slack: usize,
}

impl<W: Write> SlackPad<W> {
    const fn new(inner: W, slack: usize) -> Self {
        Self { inner, slack }
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for SlackPad<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.slack > 0
            && let Some(at) = buf.iter().position(|byte| *byte == b'\n')
        {
            let (head, tail) = buf.split_at(at);
            self.inner.write_all(head)?;
            self.inner.write_all(&vec![b' '; self.slack])?;
            self.slack = 0;
            self.inner.write_all(tail)?;
            return Ok(buf.len());
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A live session recording: an asciicast being written as the session runs.
///
/// The sink type is generic only so tests can substitute an in-memory buffer
/// and a deliberately-failing writer; production is always the defaulted
/// `BufWriter<File>` that [`SessionRecorder::create`] builds. `Seek` is part
/// of the bound because [`SessionRecorder::finish_in_place`] rewrites the
/// header line in place.
pub struct SessionRecorder<W: Write + Seek = BufWriter<File>> {
    /// `None` once the recording has been abandoned (a latched write failure)
    /// or consumed by [`SessionRecorder::finish_in_place`].
    writer: Option<CastWriter<SlackPad<W>>>,
    /// Wall clock the event timestamps are measured from.
    start: Instant,
    /// The last size written into the cast; the comparison base for
    /// [`SessionRecorder::poll_resize`].
    dims: (u16, u16),
    /// Set the first time a write fails, so the `tracing::warn!` fires once
    /// and never again on a per-frame path.
    failed: bool,
    /// The exact header line as it was serialized, without its newline. Kept
    /// so `finish` can rebuild it with `duration` appended and know how many
    /// bytes it may occupy.
    header_line: String,
    version: CastVersion,
    probe: SizeProbe,
}

impl<W: Write + Seek> std::fmt::Debug for SessionRecorder<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRecorder")
            .field("recording", &self.writer.is_some())
            .field("dims", &self.dims)
            .field("failed", &self.failed)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl SessionRecorder<BufWriter<File>> {
    /// Open `path` and write the asciicast header, sized from the outer
    /// terminal.
    ///
    /// Called before the TUI comes up, on the cooked terminal, so a bad path
    /// surfaces as an ordinary CLI error rather than a failure behind the alt
    /// screen. When stdout is not a TTY the header falls back to 80x24 — the
    /// same default the driver's viewport read uses.
    pub fn create(
        path: &Path,
        title: Option<&str>,
        version: CastVersion,
    ) -> Result<Self, AttachError> {
        let dims = tty_winsize().unwrap_or((80, 24));
        Self::open(BufWriter::new(File::create(path)?), dims, title, version)
    }
}

impl<W: Write + Seek> SessionRecorder<W> {
    /// Write the header into `sink` and return a recorder ready for bytes.
    fn open(
        sink: W,
        dims: (u16, u16),
        title: Option<&str>,
        version: CastVersion,
    ) -> Result<Self, AttachError> {
        let header = CastHeader {
            cols: dims.0,
            rows: dims.1,
            timestamp: Some(unix_now()),
            // The live tee never clamps: the .cast is the archival artifact
            // and idle collapsing belongs to the export step, which can be
            // re-run with different settings against the same capture.
            idle_time_limit: None,
            command: None,
            title: title.map(ToOwned::to_owned),
            env: recorded_env(),
            // The outer terminal's palette is not knowable from here without
            // an OSC round trip on a terminal we are about to take over, so
            // the exporter supplies its own theme instead.
            theme: None,
        };

        // Serialize the header once into a throwaway sink so `finish` knows
        // the exact byte length it is allowed to rewrite. Doing it this way
        // rather than reimplementing the serializer means the two can never
        // disagree about key order or formatting.
        let probe_bytes = CastWriter::new(Vec::new(), &header, version)
            .and_then(CastWriter::finish)
            .map_err(record_err)?;
        let header_line = String::from_utf8_lossy(&probe_bytes)
            .trim_end_matches('\n')
            .to_owned();

        // v3 has no `duration` key at all, so there is nothing to rewrite and
        // no reason to pad.
        let slack = if version == CastVersion::V2 {
            HEADER_SLACK
        } else {
            0
        };
        let writer =
            CastWriter::new(SlackPad::new(sink, slack), &header, version).map_err(record_err)?;

        Ok(Self {
            writer: Some(writer),
            start: Instant::now(),
            dims,
            failed: false,
            header_line,
            version,
            probe: Box::new(tty_winsize),
        })
    }

    /// Test seam: build a recorder over an arbitrary sink with fixed initial
    /// dimensions, so no test depends on the size of the terminal it runs in.
    #[cfg(test)]
    fn with_writer(
        sink: W,
        cols: u16,
        rows: u16,
        title: Option<&str>,
        version: CastVersion,
    ) -> Result<Self, AttachError> {
        Self::open(sink, (cols, rows), title, version)
    }

    /// Test seam: replace the winsize probe with a scripted one.
    #[cfg(test)]
    #[must_use]
    fn with_size_probe(mut self, probe: SizeProbe) -> Self {
        self.probe = probe;
        self
    }

    /// Record composited output bytes, timestamped now.
    ///
    /// Infallible by design: see the module's failure policy. A partial or
    /// failed write abandons the recording rather than propagating an error
    /// into the render path.
    pub fn record(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let at = self.start.elapsed();
        let outcome = self.writer.as_mut().map(|w| w.output(at, bytes));
        if let Some(Err(err)) = outcome {
            self.abandon(&err);
        }
    }

    /// Emit a resize event if the outer terminal changed size since the last
    /// check.
    ///
    /// Comparing before emitting is load-bearing: this runs once per frame
    /// flush, and an unconditional `"r"` would bury the recording under
    /// thousands of no-op resizes that make a player rebuild its canvas.
    pub fn poll_resize(&mut self) {
        let Some(dims) = (self.probe)() else {
            return;
        };
        if dims == self.dims {
            return;
        }
        self.dims = dims;
        let at = self.start.elapsed();
        let outcome = self.writer.as_mut().map(|w| w.resize(at, dims.0, dims.1));
        if let Some(Err(err)) = outcome {
            self.abandon(&err);
        }
    }

    /// Flush the residual UTF-8 tail, backfill the header's `duration`, and
    /// close the file in place.
    ///
    /// Idempotent so the production pre-`process::exit` path and the CLI's
    /// returning fallback can safely share an `Rc<RefCell<_>>`. Once called,
    /// later recording writes and repeated finalization calls are no-ops.
    pub fn finish_in_place(&mut self) -> Result<(), AttachError> {
        let Some(writer) = self.writer.take() else {
            // Already abandoned. The prefix on disk is a valid, playable
            // asciicast; it simply has no `duration`, which is exactly what a
            // truncated recording should look like.
            return Ok(());
        };
        let elapsed_ms = writer.elapsed_ms();
        let padded = writer.finish().map_err(record_err)?;
        let mut sink = padded.into_inner();

        if let Some(line) = self.duration_header(elapsed_ms) {
            sink.seek(SeekFrom::Start(0))?;
            sink.write_all(&line)?;
        }
        sink.flush()?;
        Ok(())
    }

    /// Consuming convenience wrapper around [`Self::finish_in_place`].
    pub fn finish(mut self) -> Result<(), AttachError> {
        self.finish_in_place()
    }

    /// Build the replacement header line carrying `duration`, padded back out
    /// to the exact width reserved at open time.
    ///
    /// Returns `None` when there is nothing safe to write: v3 (no `duration`
    /// key exists), an unexpected header shape, or a line that somehow
    /// outgrew its slack. Every one of those is "skip the rewrite", never
    /// "corrupt the file".
    fn duration_header(&self, elapsed_ms: u64) -> Option<Vec<u8>> {
        if self.version != CastVersion::V2 {
            return None;
        }
        let body = self.header_line.strip_suffix('}')?;
        let line = format!(
            "{body},\"duration\":{}.{:03}}}",
            elapsed_ms / 1000,
            elapsed_ms % 1000
        );
        let width = self.header_line.len().checked_add(HEADER_SLACK)?;
        if line.len() > width {
            return None;
        }
        let mut bytes = line.into_bytes();
        bytes.resize(width, b' ');
        Some(bytes)
    }

    /// Give up on the recording after a write failure, warning exactly once.
    fn abandon(&mut self, err: &RecordError) {
        if !self.failed {
            self.failed = true;
            // File-only sink: the alt screen is up and stderr is off limits.
            tracing::warn!(error = %err, "session recording stopped after a write failure");
        }
        self.writer = None;
    }
}

/// The `RenderSink` wrapper that feeds a [`SessionRecorder`] on the way past.
///
/// Generic over the recorder's sink type purely so tests can drive a failing
/// recorder; the default is the production `BufWriter<File>`, so the driver
/// names this as `TeeSink { inner, rec }` with no type arguments.
pub(crate) struct TeeSink<'a, W: Write, R: Write + Seek = BufWriter<File>> {
    pub(crate) inner: &'a mut W,
    pub(crate) rec: Rc<RefCell<SessionRecorder<R>>>,
}

impl<W: Write, R: Write + Seek> std::fmt::Debug for TeeSink<'_, W, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeeSink").finish_non_exhaustive()
    }
}

impl<W: Write, R: Write + Seek> Write for TeeSink<'_, W, R> {
    /// Write through, then record **only the bytes the inner sink accepted**.
    ///
    /// The order matters. Recording the whole buffer after a short write
    /// would put bytes in the recording that the glass never received, and
    /// every later byte would land at the wrong offset — the recording would
    /// diverge from what the human actually saw and stay diverged.
    ///
    /// `write_all` is deliberately NOT overridden: its default implementation
    /// loops on `write`, so the short-write accounting above stays the single
    /// place that decides what gets recorded.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let taken = self.inner.write(buf)?;
        if let Some(accepted) = buf.get(..taken) {
            self.rec.borrow_mut().record(accepted);
        }
        Ok(taken)
    }

    /// Flush is the frame boundary, so it is also where the recorder checks
    /// for a resize — no SIGWINCH plumbing into the driver required.
    fn flush(&mut self) -> io::Result<()> {
        self.rec.borrow_mut().poll_resize();
        self.inner.flush()
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
    use phux_record::cast::{EventCode, read_cast};

    /// Shared in-memory stand-in for `BufWriter<File>`.
    ///
    /// Implements `Seek` because `finish` rewrites the header in place; a
    /// plain `Vec<u8>` would not exercise that path at all. `fail` flips the
    /// sink into a permanent `ErrorKind::Other`, standing in for a full disk.
    #[derive(Clone, Default)]
    struct MemSink(Rc<RefCell<MemState>>);

    #[derive(Default)]
    struct MemState {
        buf: Vec<u8>,
        pos: usize,
        fail: bool,
    }

    impl MemSink {
        fn contents(&self) -> Vec<u8> {
            self.0.borrow().buf.clone()
        }

        fn set_failing(&self, failing: bool) {
            self.0.borrow_mut().fail = failing;
        }
    }

    impl Write for MemSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut state = self.0.borrow_mut();
            if state.fail {
                return Err(io::Error::other("simulated full disk"));
            }
            let pos = state.pos;
            let end = pos + buf.len();
            if state.buf.len() < end {
                state.buf.resize(end, 0);
            }
            state.buf[pos..end].copy_from_slice(buf);
            state.pos = end;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.0.borrow().fail {
                return Err(io::Error::other("simulated full disk"));
            }
            Ok(())
        }
    }

    impl Seek for MemSink {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            match from {
                SeekFrom::Start(at) => {
                    let pos = usize::try_from(at).map_err(io::Error::other)?;
                    self.0.borrow_mut().pos = pos;
                    Ok(at)
                }
                // The recorder only ever seeks to the header; anything else is
                // a bug we want to see loudly rather than emulate.
                _ => Err(io::Error::other("MemSink supports SeekFrom::Start only")),
            }
        }
    }

    /// A sink that accepts at most `limit` bytes per call — the short-write
    /// terminal the tee must not desynchronize against.
    struct ShortWriter {
        got: Vec<u8>,
        limit: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let take = buf.len().min(self.limit);
            self.got.extend_from_slice(&buf[..take]);
            Ok(take)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn recorder(sink: MemSink) -> SessionRecorder<MemSink> {
        SessionRecorder::with_writer(sink, 80, 24, Some("t"), CastVersion::V2)
            .expect("recorder opens")
    }

    /// Concatenate every `o` event's payload from a finished cast.
    fn output_text(bytes: &[u8]) -> String {
        let (_, events) = read_cast(bytes).expect("cast parses");
        events
            .iter()
            .filter(|e| e.code == EventCode::Output)
            .map(|e| e.data.clone())
            .collect()
    }

    #[test]
    fn tee_records_exactly_the_bytes_the_inner_sink_accepted() {
        let sink = MemSink::default();
        let rec = Rc::new(RefCell::new(recorder(sink.clone())));
        let mut inner = ShortWriter {
            got: Vec::new(),
            limit: 3,
        };
        {
            let mut tee = TeeSink {
                inner: &mut inner,
                rec: Rc::clone(&rec),
            };
            // One raw `write` of 10 bytes: the inner sink takes 3, so exactly
            // 3 must reach the recording.
            let taken = tee.write(b"abcdefghij").expect("tee write");
            assert_eq!(taken, 3, "short write is reported honestly");
        }
        Rc::try_unwrap(rec)
            .expect("sole owner")
            .into_inner()
            .finish()
            .expect("finish");
        assert_eq!(
            output_text(&sink.contents()),
            "abc",
            "the recording must hold the accepted prefix, never the whole buffer"
        );
        assert_eq!(inner.got, b"abc");
    }

    #[test]
    fn tee_forwards_every_byte_to_the_inner_sink() {
        let sink = MemSink::default();
        let rec = Rc::new(RefCell::new(recorder(sink.clone())));
        let mut inner = ShortWriter {
            got: Vec::new(),
            limit: 4,
        };
        {
            let mut tee = TeeSink {
                inner: &mut inner,
                rec: Rc::clone(&rec),
            };
            // `write_all` loops over the short writes; both sides must end up
            // with the identical byte stream.
            tee.write_all(b"\x1b[2Jhello world").expect("tee write_all");
        }
        Rc::try_unwrap(rec)
            .expect("sole owner")
            .into_inner()
            .finish()
            .expect("finish");
        assert_eq!(inner.got, b"\x1b[2Jhello world");
        assert_eq!(output_text(&sink.contents()), "\x1b[2Jhello world");
    }

    #[test]
    fn flush_emits_a_resize_event_when_the_probe_reports_new_dims() {
        let sink = MemSink::default();
        // Popped from the end: first probe agrees with the header, second
        // reports a genuine resize.
        let mut scripted = vec![(100_u16, 30_u16), (80, 24)];
        let rec = Rc::new(RefCell::new(
            recorder(sink.clone()).with_size_probe(Box::new(move || scripted.pop())),
        ));
        let mut inner = Vec::new();
        {
            let mut tee = TeeSink {
                inner: &mut inner,
                rec: Rc::clone(&rec),
            };
            tee.flush().expect("first flush");
            tee.flush().expect("second flush");
        }
        Rc::try_unwrap(rec)
            .expect("sole owner")
            .into_inner()
            .finish()
            .expect("finish");
        let (_, events) = read_cast(sink.contents().as_slice()).expect("cast parses");
        let resizes: Vec<_> = events
            .iter()
            .filter(|e| e.code == EventCode::Resize)
            .collect();
        assert_eq!(resizes.len(), 1, "only the real size change is recorded");
        assert_eq!(resizes[0].data, "100x30");
    }

    #[test]
    fn flush_emits_no_resize_event_when_dims_are_unchanged() {
        let sink = MemSink::default();
        let rec = Rc::new(RefCell::new(
            recorder(sink.clone()).with_size_probe(Box::new(|| Some((80, 24)))),
        ));
        let mut inner = Vec::new();
        {
            let mut tee = TeeSink {
                inner: &mut inner,
                rec: Rc::clone(&rec),
            };
            for _ in 0..5 {
                tee.flush().expect("flush");
            }
        }
        Rc::try_unwrap(rec)
            .expect("sole owner")
            .into_inner()
            .finish()
            .expect("finish");
        let (_, events) = read_cast(sink.contents().as_slice()).expect("cast parses");
        assert!(
            events.iter().all(|e| e.code != EventCode::Resize),
            "a per-frame ioctl must not produce per-frame resize events"
        );
    }

    #[test]
    fn recorder_writes_a_parsable_v2_header_with_the_initial_dims() {
        let sink = MemSink::default();
        let mut rec =
            SessionRecorder::with_writer(sink.clone(), 120, 40, Some("demo"), CastVersion::V2)
                .expect("recorder opens");
        rec.record(b"hi");
        rec.finish().expect("finish");

        let (header, events) = read_cast(sink.contents().as_slice()).expect("cast parses");
        assert_eq!(header.cols, 120);
        assert_eq!(header.rows, 40);
        assert_eq!(header.title.as_deref(), Some("demo"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn finish_writes_a_duration_field() {
        let sink = MemSink::default();
        let mut rec = recorder(sink.clone());
        rec.record(b"x");
        rec.finish().expect("finish");

        let raw = sink.contents();
        let text = String::from_utf8(raw).expect("utf-8");
        let head = text.lines().next().expect("header line");
        assert!(
            head.contains("\"duration\":"),
            "header must be backfilled in place: {head}"
        );
        // The rewrite must not have eaten into the first event line.
        let (_, events) = read_cast(text.as_bytes()).expect("cast still parses");
        assert_eq!(events.len(), 1, "the event stream survives the rewrite");
        assert_eq!(events[0].data, "x");
    }

    #[test]
    fn finish_in_place_is_idempotent_and_stops_future_writes() {
        let sink = MemSink::default();
        let mut rec = recorder(sink.clone());
        rec.record(b"complete");

        rec.finish_in_place().expect("first finish");
        let finished = sink.contents();
        let text = String::from_utf8(finished.clone()).expect("utf-8");
        assert_eq!(
            text.matches("\"duration\":").count(),
            1,
            "duration is backfilled exactly once"
        );

        rec.record(b"must not be appended");
        rec.finish_in_place().expect("repeated finish");
        assert_eq!(
            sink.contents(),
            finished,
            "writes and finalization after finish must leave the cast unchanged"
        );
        let (_, events) = read_cast(finished.as_slice()).expect("cast still parses");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "complete");
    }

    #[test]
    fn recorder_write_failure_is_logged_once_and_then_silent() {
        let sink = MemSink::default();
        let rec = Rc::new(RefCell::new(recorder(sink.clone())));
        let mut inner = Vec::new();
        {
            let mut tee = TeeSink {
                inner: &mut inner,
                rec: Rc::clone(&rec),
            };
            tee.write_all(b"before").expect("write before failure");
            sink.set_failing(true);
            tee.write_all(b"during")
                .expect("a full disk must not fail the glass");
            // Even once the sink recovers, the recording stays abandoned: the
            // cast on disk would otherwise have a hole in the middle.
            sink.set_failing(false);
            tee.write_all(b"after").expect("write after failure");
        }
        assert_eq!(
            inner, b"beforeduringafter",
            "the user's terminal keeps receiving every byte"
        );
        assert!(
            rec.borrow().failed,
            "the failure latch fires so the warn is emitted exactly once"
        );
        Rc::try_unwrap(rec)
            .expect("sole owner")
            .into_inner()
            .finish()
            .expect("finish on an abandoned recording is not an error");
    }

    #[test]
    fn abandoned_recording_leaves_a_playable_prefix() {
        let sink = MemSink::default();
        let mut rec = recorder(sink.clone());
        rec.record(b"kept");
        sink.set_failing(true);
        rec.record(b"lost");
        sink.set_failing(false);
        rec.finish().expect("finish");

        let (header, events) = read_cast(sink.contents().as_slice()).expect("prefix parses");
        assert_eq!(header.cols, 80);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "kept");
    }

    #[test]
    fn v3_recording_omits_the_duration_rewrite() {
        let sink = MemSink::default();
        let mut rec = SessionRecorder::with_writer(sink.clone(), 80, 24, None, CastVersion::V3)
            .expect("recorder opens");
        rec.record(b"x");
        rec.finish().expect("finish");

        let text = String::from_utf8(sink.contents()).expect("utf-8");
        let head = text.lines().next().expect("header line");
        assert!(!head.contains("duration"), "v3 has no duration key: {head}");
        assert!(
            !head.ends_with(' '),
            "v3 reserves no slack, so no padding is written: {head:?}"
        );
    }

    #[test]
    fn create_opens_a_file_and_writes_a_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.cast");
        let mut rec =
            SessionRecorder::create(&path, Some("live"), CastVersion::V2).expect("create");
        rec.record(b"ok");
        rec.finish().expect("finish");

        let bytes = std::fs::read(&path).expect("read back");
        let (header, events) = read_cast(bytes.as_slice()).expect("cast parses");
        assert!(header.cols > 0 && header.rows > 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "ok");
    }
}
