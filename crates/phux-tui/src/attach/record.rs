//! The `phux --rec` session tee: an asciicast written from the client's own
//! composited output stream (ADR-0060).
//!
//! It wraps the one `RenderSink` the driver threads through the whole render
//! path, so the recording is byte-for-byte what the glass received, chrome
//! included, and it sits upstream of `StdoutSink`'s backlog drop, so a
//! backpressured terminal loses frames on screen but not in the recording.
//! Timing is the paint cadence (bursts coalesce into one event); content is
//! exact.
//!
//! Nothing here may kill the session or write to stderr (it would corrupt
//! the alt screen): a write failure latches one `tracing::warn!` and goes
//! silent, leaving a playable prefix on disk.

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

/// Trailing whitespace reserved after the header line so `finish` can
/// rewrite it in place with `,"duration":...` (22 bytes for a day).
const HEADER_SLACK: usize = 32;

/// How the recorder learns the outer terminal's size (boxed so the
/// `Rc<RefCell<_>>` the driver holds needs no extra type parameter).
type SizeProbe = Box<dyn FnMut() -> Option<(u16, u16)>>;

/// The controlling TTY's size, or `None` when stdout is not a terminal
/// (one ioctl per frame flush).
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

/// The environment asciinema records: just these two, since a recording is
/// shareable and a full `environ` dump leaks tokens.
fn recorded_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["TERM", "SHELL"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_owned(), value);
        }
    }
    env
}

/// Recording failures collapse onto [`AttachError::Io`].
fn record_err(err: RecordError) -> AttachError {
    match err {
        RecordError::Io(inner) => AttachError::Io(inner),
        other => AttachError::Io(io::Error::other(other.to_string())),
    }
}

/// Pads the header line with trailing spaces so `finish` can seek back and
/// overwrite it with the `duration` key (known only at the end) instead of
/// rewriting the whole file. JSON readers ignore the whitespace.
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

/// A live session recording. Generic only so tests can use an in-memory
/// (or failing) sink; `Seek` lets `finish` rewrite the header in place.
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
    /// The serialized header line (no newline), rebuilt with `duration` at
    /// finish within its reserved width.
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
    /// Open `path` and write the header sized from the outer terminal (80x24
    /// when stdout is not a TTY). Runs before the TUI comes up, so a bad path
    /// is an ordinary CLI error.
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
            // Idle clamping belongs to the export step, not the archive.
            idle_time_limit: None,
            command: None,
            title: title.map(ToOwned::to_owned),
            env: recorded_env(),
            // The outer palette is unknowable here; the exporter themes.
            theme: None,
        };

        // Serialize once into a throwaway sink so `finish` knows the exact
        // byte length it may rewrite, without a second serializer.
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

    /// Record composited output bytes, timestamped now. Infallible: a failed
    /// write abandons the recording.
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

    /// Emit a resize event only when the outer terminal's size changed
    /// (this runs every flush).
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
    /// close. Idempotent, so the pre-exit path and the CLI fallback can share
    /// it.
    pub fn finish_in_place(&mut self) -> Result<(), AttachError> {
        let Some(writer) = self.writer.take() else {
            // Abandoned: the prefix on disk is a playable asciicast without a
            // duration.
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

    /// The replacement header line with `duration`, padded to the reserved
    /// width, or `None` when a rewrite would not be safe (v3, an unexpected
    /// shape, or no room).
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
    /// Write through, then record only the bytes the inner sink accepted, so
    /// a short write never puts bytes in the recording the glass never got.
    /// `write_all`'s default loops on this, keeping one accounting point.
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

    /// Shared in-memory `Seek` sink; `fail` makes it error like a full disk.
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
