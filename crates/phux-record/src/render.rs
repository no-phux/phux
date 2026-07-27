//! The two-pass render driver: cast events in, animation out.
//!
//! [`render_cast`] replays the event list **twice**. Pass 1 builds the color
//! histogram that becomes GIF's global color table and counts the frames that
//! APNG's `acTL` needs up front; pass 2 replays from scratch and encodes.
//! Replay is deterministic, so the two passes agree by construction, and
//! memory stays O(one surface) rather than O(all frames) — which is the whole
//! reason it is two passes and not one buffered one.
//!
//! Both passes drive the same [`SampleWalk`]. That is not a tidiness
//! preference: if the two passes ever disagreed about how many frames a
//! recording produces, APNG's `acTL` would promise a count the file does not
//! contain, and the only honest way to guarantee they agree is for there to
//! be exactly one implementation of the walk.
//!
//! # Sampling
//!
//! Fixed-period, never per-event. `period_ms = 1000 / fps` with `fps`
//! normalized to one of `{5, 10, 20, 25, 50}` so the period divides 1000
//! exactly: exact integer periods mean zero accumulated drift and, for GIF,
//! exact centisecond delays. The driver maintains `next_sample = k *
//! period_ms` on the cast timeline, feeds every event at or before it, then
//! samples once. A sample that reports clean emits **no frame at all** — the
//! period folds into the previous frame's pending delay, so an idle terminal
//! costs nothing.
//!
//! # Why a frame is emitted one sample late
//!
//! A frame's delay is how long it stays on screen, which is not known until
//! the *next* frame arrives. The walk therefore holds one sampled frame back,
//! accumulating its delay, and hands it to the caller when the following
//! frame displaces it. Exactly one frame is buffered, so this costs one grid
//! of cells and not a film.

use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use crate::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode};
use crate::encode::{AnimEncoder, CountingWriter, Palette, Rect};
use crate::error::RecordError;
use crate::raster::{Rasterizer, Surface, Theme};
use crate::replay::{Replayer, Sampled};
use crate::timeline::clamp_idle;

/// Longest single accumulated delay, in milliseconds.
///
/// A pause longer than this reads as a stall rather than a pause, and GIF
/// cannot express more than 500 cs anyway.
const MAX_DELAY_MS: u32 = 5_000;

/// Force a whole-canvas frame this often.
///
/// Sub-rectangle frames are differential: a file truncated partway through
/// decodes to whatever the last keyframe plus the surviving deltas describe.
/// A periodic keyframe bounds how stale that can get when `--max-bytes` cuts
/// a render short.
const KEYFRAME_INTERVAL: u32 = 100;

/// What a recording is exported as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Re-serialize as an asciicast. This is the archival artifact and the
    /// `--from a.cast -o b.cast` transcode path.
    Cast,
    /// Animated `GIF89a`: the shareable, embeddable default.
    #[default]
    Gif,
    /// Animated PNG: truecolor, no quantization, 1 ms timing resolution.
    Apng,
}

impl OutputFormat {
    /// The lowercase name used in `--json` output and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cast => "cast",
            Self::Gif => "gif",
            Self::Apng => "apng",
        }
    }
}

/// Knobs for an animation render.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Animation sample rate, normalized to a period dividing 1000 ms.
    pub fps: u8,
    /// Collapse any pause longer than this many seconds down to it.
    pub idle_limit: Option<f64>,
    /// Stop encoding and report `truncated` once the output reaches this
    /// many bytes. Never fill the user's disk, and never panic.
    pub max_bytes: u64,
    /// Override the theme instead of reading it from the replayed terminal.
    pub theme: Option<Theme>,
    /// Hold the final frame this long so viewers see the end state before
    /// the loop wraps.
    pub tail_hold_ms: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            fps: 10,
            idle_limit: Some(2.0),
            max_bytes: 8 * 1024 * 1024,
            theme: None,
            tail_hold_ms: 2000,
        }
    }
}

/// What a render produced, for the CLI's one-liner and `--json` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Frames actually emitted.
    pub frames: u32,
    /// Bytes written to the sink.
    pub bytes: u64,
    /// Wall duration of the rendered timeline, after idle clamping.
    pub duration_ms: u64,
    /// Distinct colors seen, which is how one tells the lossless palette
    /// path from the nearest-fit fallback.
    pub colors: u32,
    /// Whether `max_bytes` cut the render short.
    pub truncated: bool,
}

/// Render `events` into `sink` as `format`.
///
/// # Errors
///
/// Propagates replay, encode, and I/O failures. A `max_bytes` overrun is not
/// an error: the container is closed cleanly and `RenderStats::truncated` is
/// set, because a short animation the user can still open beats a failed
/// command and a half-written file.
pub fn render_cast<W: Write>(
    header: &CastHeader,
    events: &[CastEvent],
    sink: W,
    format: OutputFormat,
    opts: &RenderOptions,
) -> Result<RenderStats, RecordError> {
    // The idle clamp is applied to the shared event list before anything is
    // written, so a `.cast` and a GIF rendered from the same options can never
    // disagree about when something happened.
    let mut events = events.to_vec();
    clamp_idle(&mut events, opts.idle_limit);
    let duration_ms = events.last().map_or(0, |event| event.time_ms);

    if format == OutputFormat::Cast {
        return transcode(header, &events, sink, duration_ms);
    }

    let pass1 = survey(header, &events, opts)?;
    encode(header, &events, sink, format, opts, &pass1, duration_ms)
}

/// Re-serialize a header and event list as an asciicast.
///
/// Version 2 by design and not by omission: upstream states v3 is *not*
/// backward compatible with v1/v2, and there is no consumer that reads v3 but
/// not v2 (ADR-0060). A caller that wants v3 drives [`CastWriter`] directly
/// with [`CastVersion::V3`], which is what the CLI does when the user asks
/// for it explicitly.
fn transcode<W: Write>(
    header: &CastHeader,
    events: &[CastEvent],
    sink: W,
    duration_ms: u64,
) -> Result<RenderStats, RecordError> {
    let count = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let mut writer = CastWriter::new(
        CountingWriter::new(sink, std::rc::Rc::clone(&count)),
        header,
        CastVersion::V2,
    )?;
    let mut written = 0_u32;
    for event in events {
        let at = Duration::from_millis(event.time_ms);
        match event.code {
            EventCode::Output => writer.output(at, event.data.as_bytes())?,
            EventCode::Resize => {
                let (cols, rows) = parse_resize(&event.data).ok_or_else(|| {
                    RecordError::Cast(format!(
                        "resize event data {:?} is not COLSxROWS",
                        event.data
                    ))
                })?;
                writer.resize(at, cols, rows)?;
            }
            EventCode::Marker => writer.marker(at, &event.data)?,
            EventCode::Exit => {
                // A non-numeric status is not worth failing a transcode over;
                // 0 is the only defensible substitute and the recording is
                // still complete without it.
                writer.exit(at, event.data.trim().parse::<i32>().unwrap_or(0))?;
            }
            // Input is never re-emitted. Both spec versions tell recorders not
            // to capture it, and a transcode that resurrected it from a
            // hand-edited file would put keystrokes back on disk.
            EventCode::Input => continue,
        }
        written = written.saturating_add(1);
    }
    writer.finish()?;
    Ok(RenderStats {
        frames: written,
        bytes: count.get(),
        duration_ms,
        colors: 0,
        truncated: false,
    })
}

/// What pass 1 learned about the recording.
#[derive(Debug)]
struct Survey {
    frames: u32,
    colors: HashSet<[u8; 3]>,
    theme: Theme,
    cols: u16,
    rows: u16,
}

/// Pass 1: count frames, collect colors, and find the largest grid.
fn survey(
    header: &CastHeader,
    events: &[CastEvent],
    opts: &RenderOptions,
) -> Result<Survey, RecordError> {
    let mut walk = SampleWalk::new(
        header,
        events,
        normalize_period_ms(opts.fps),
        opts.tail_hold_ms,
    )?;
    let mut colors = HashSet::new();
    let mut frames = 0_u32;
    let mut rasterizer: Option<Rasterizer> = None;

    while let Some((sampled, _delay)) = walk.next_frame()? {
        // The theme is read immediately after the first sample, which is the
        // point `Replayer::theme` documents as the cheap one: the answer has
        // settled and the render state is about to be drained anyway.
        let raster = match rasterizer.as_ref() {
            Some(existing) => existing,
            None => rasterizer.insert(Rasterizer::new(match opts.theme.clone() {
                Some(theme) => theme,
                None => walk.theme()?,
            })),
        };
        raster.colors_of(&sampled.frame, &mut colors);
        frames = frames.saturating_add(1);
    }

    let theme = rasterizer.map_or_else(
        // No frames at all means an empty recording; the caller's theme, or
        // the default table, is still the right answer for the canvas fill.
        || opts.theme.clone().unwrap_or_default(),
        |raster| raster.theme().clone(),
    );
    // Even an empty recording gets a background: the canvas has to exist for
    // the container header to be writable at all.
    colors.insert(theme.bg);
    Ok(Survey {
        frames,
        colors,
        theme,
        cols: walk.max_cols.max(1),
        rows: walk.max_rows.max(1),
    })
}

/// Pass 2: replay from scratch and encode.
fn encode<W: Write>(
    header: &CastHeader,
    events: &[CastEvent],
    sink: W,
    format: OutputFormat,
    opts: &RenderOptions,
    pass1: &Survey,
    duration_ms: u64,
) -> Result<RenderStats, RecordError> {
    let rasterizer = Rasterizer::new(pass1.theme.clone());
    let mut surface = rasterizer.surface_for(pass1.cols, pass1.rows);
    let (_cell_w, cell_h) = rasterizer.cell_size();

    let mut encoder = match format {
        OutputFormat::Gif => AnimEncoder::gif(
            sink,
            surface.width,
            surface.height,
            Palette::build(&pass1.colors, &pass1.theme),
        )?,
        // `acTL` needs the count before the header goes out; that requirement
        // is the reason pass 1 exists.
        OutputFormat::Apng => {
            AnimEncoder::apng(sink, surface.width, surface.height, pass1.frames.max(1))?
        }
        OutputFormat::Cast => {
            return Err(RecordError::Unsupported(
                "cast output does not go through the animation encoder".into(),
            ));
        }
    };

    let mut walk = SampleWalk::new(
        header,
        events,
        normalize_period_ms(opts.fps),
        opts.tail_hold_ms,
    )?;
    let mut frames = 0_u32;
    let mut truncated = false;

    while let Some((sampled, delay_ms)) = walk.next_frame()? {
        rasterizer.draw(&sampled.frame, &mut surface);
        let rect = frame_rect(&sampled, &surface, cell_h, frames);
        let bytes = encoder.add_frame(&surface, rect, delay_ms)?;
        frames = frames.saturating_add(1);
        if bytes >= opts.max_bytes {
            // Stop adding frames but still close the container: a truncated
            // animation the user can open beats a half-written file and a
            // failed command.
            truncated = true;
            break;
        }
    }

    let bytes = encoder.finish()?;
    Ok(RenderStats {
        frames,
        bytes,
        duration_ms,
        colors: u32::try_from(pass1.colors.len()).unwrap_or(u32::MAX),
        truncated,
    })
}

/// The rectangle a sampled frame should be encoded as.
///
/// Full-width rows only. Per-row column bounds would shave a little more off
/// a single blinking cursor, and would add a second dimension of off-by-one
/// to every wide-glyph and decoration case for it.
fn frame_rect(sampled: &Sampled, surface: &Surface, cell_h: u32, emitted: u32) -> Rect {
    let keyframe = emitted.is_multiple_of(KEYFRAME_INTERVAL);
    match sampled.dirty_rows {
        Some((first, last)) if !keyframe => {
            let y = u32::from(first).saturating_mul(cell_h);
            let span = u32::from(last.saturating_sub(first).saturating_add(1));
            Rect {
                x: 0,
                y,
                w: surface.width,
                h: span.saturating_mul(cell_h),
            }
        }
        _ => Rect::whole(surface),
    }
}

/// Drives the replayer on the export's fixed sample clock.
///
/// Both passes construct one of these over the same inputs. Replay is
/// deterministic, so two walks over one event list produce the same frames in
/// the same order with the same delays — which is what lets pass 1's count be
/// handed to APNG's `acTL` as a promise.
struct SampleWalk<'a> {
    events: &'a [CastEvent],
    replayer: Replayer,
    /// Index of the next event to feed.
    idx: usize,
    /// The sample instant, on the cast's own millisecond timeline.
    next_ms: u64,
    period_ms: u32,
    tail_hold_ms: u32,
    /// The frame whose delay is still accumulating.
    pending: Option<Sampled>,
    pending_delay_ms: u32,
    /// Set once the event list is exhausted; the next call flushes `pending`.
    done: bool,
    /// Largest grid seen, so the canvas never has to grow mid-animation.
    max_cols: u16,
    max_rows: u16,
}

impl std::fmt::Debug for SampleWalk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SampleWalk")
            .field("events", &self.events.len())
            .field("idx", &self.idx)
            .field("next_ms", &self.next_ms)
            .field("period_ms", &self.period_ms)
            .field("max_cols", &self.max_cols)
            .field("max_rows", &self.max_rows)
            .finish_non_exhaustive()
    }
}

impl<'a> SampleWalk<'a> {
    fn new(
        header: &CastHeader,
        events: &'a [CastEvent],
        period_ms: u32,
        tail_hold_ms: u32,
    ) -> Result<Self, RecordError> {
        // A header claiming a zero dimension is malformed, but refusing to
        // export it helps nobody: 80x24 is the universal fallback and the
        // recording's own resize events will correct it within one frame.
        let cols = if header.cols == 0 { 80 } else { header.cols };
        let rows = if header.rows == 0 { 24 } else { header.rows };
        Ok(Self {
            events,
            replayer: Replayer::new(cols, rows)?,
            idx: 0,
            next_ms: 0,
            period_ms: period_ms.max(1),
            tail_hold_ms,
            pending: None,
            pending_delay_ms: 0,
            done: false,
            max_cols: cols,
            max_rows: rows,
        })
    }

    /// The replayed terminal's own color table.
    fn theme(&mut self) -> Result<Theme, RecordError> {
        self.replayer.theme()
    }

    /// The next frame whose delay is now known, or `None` at the end.
    fn next_frame(&mut self) -> Result<Option<(Sampled, u32)>, RecordError> {
        loop {
            if self.done {
                // The final frame is held for `tail_hold_ms` on top of
                // whatever idle time it already accumulated, so a viewer sees
                // the end state before the loop wraps back to a blank screen.
                let hold = self
                    .pending_delay_ms
                    .min(MAX_DELAY_MS)
                    .saturating_add(self.tail_hold_ms);
                return Ok(self.pending.take().map(|sampled| (sampled, hold)));
            }

            self.feed_due_events()?;
            let sampled = self.replayer.sample()?;
            let exhausted = self.idx >= self.events.len();

            let ready = if let Some(sampled) = sampled {
                self.max_cols = self.max_cols.max(sampled.frame.cols);
                self.max_rows = self.max_rows.max(sampled.frame.rows);
                let previous = self.pending.replace(sampled);
                let delay = self.pending_delay_ms.min(MAX_DELAY_MS);
                self.pending_delay_ms = self.period_ms;
                previous.map(|frame| (frame, delay))
            } else {
                // Clean: no frame at all. The period folds into the delay of
                // the frame already on screen, which is what makes an idle
                // recording cost nothing.
                self.pending_delay_ms = self.pending_delay_ms.saturating_add(self.period_ms);
                None
            };

            if exhausted {
                self.done = true;
            } else {
                self.next_ms = self.next_ms.saturating_add(u64::from(self.period_ms));
            }
            if ready.is_some() {
                return Ok(ready);
            }
        }
    }

    /// Feed every event at or before the current sample instant.
    fn feed_due_events(&mut self) -> Result<(), RecordError> {
        while let Some(event) = self.events.get(self.idx) {
            if event.time_ms > self.next_ms {
                break;
            }
            match event.code {
                EventCode::Output => self.replayer.feed(event.data.as_bytes()),
                EventCode::Resize => {
                    if let Some((cols, rows)) = parse_resize(&event.data)
                        && cols > 0
                        && rows > 0
                    {
                        self.replayer.resize(cols, rows)?;
                        self.max_cols = self.max_cols.max(cols);
                        self.max_rows = self.max_rows.max(rows);
                    }
                }
                // Markers, exits, and input carry no pixels. A malformed
                // resize is skipped rather than fatal, matching the codec's
                // unknown-code tolerance: a recording is not worth losing over
                // one bad line.
                EventCode::Marker | EventCode::Exit | EventCode::Input => {}
            }
            self.idx = self.idx.saturating_add(1);
        }
        Ok(())
    }
}

/// Parse an asciicast resize payload, `"{COLS}x{ROWS}"`.
fn parse_resize(data: &str) -> Option<(u16, u16)> {
    let (cols, rows) = data.trim().split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// Normalize an arbitrary fps to one whose period divides 1000 ms exactly.
///
/// `{5, 10, 20, 25, 50}` fps map to `{200, 100, 50, 40, 20}` ms. Exact
/// integer periods mean the sample clock cannot drift and GIF's centisecond
/// delays come out exact rather than rounded.
#[must_use]
pub fn normalize_period_ms(fps: u8) -> u32 {
    const ALLOWED: [(u8, u32); 5] = [(5, 200), (10, 100), (20, 50), (25, 40), (50, 20)];
    let target = i32::from(fps.max(1));
    let mut best_period = 100_u32;
    let mut best_distance = i32::MAX;
    for (candidate, period) in ALLOWED {
        let distance = (i32::from(candidate) - target).abs();
        if distance < best_distance {
            best_distance = distance;
            best_period = period;
        }
    }
    best_period
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use std::io::BufReader;

    use phux_core::screen::RenderedFrame;

    use super::*;
    use crate::cast::read_cast;

    fn header(cols: u16, rows: u16) -> CastHeader {
        CastHeader {
            cols,
            rows,
            ..CastHeader::default()
        }
    }

    fn output(time_ms: u64, data: &str) -> CastEvent {
        CastEvent {
            time_ms,
            code: EventCode::Output,
            data: data.to_owned(),
        }
    }

    fn resize(time_ms: u64, cols: u16, rows: u16) -> CastEvent {
        CastEvent {
            time_ms,
            code: EventCode::Resize,
            data: format!("{cols}x{rows}"),
        }
    }

    fn walk_of<'a>(header: &CastHeader, events: &'a [CastEvent]) -> SampleWalk<'a> {
        SampleWalk::new(header, events, 100, 0).expect("walk builds")
    }

    fn frames_of(header: &CastHeader, events: &[CastEvent]) -> Vec<(Sampled, u32)> {
        let mut walk = walk_of(header, events);
        let mut out = Vec::new();
        while let Some(frame) = walk.next_frame().expect("walk advances") {
            out.push(frame);
        }
        out
    }

    fn render_to_vec(
        header: &CastHeader,
        events: &[CastEvent],
        format: OutputFormat,
        opts: &RenderOptions,
    ) -> (Vec<u8>, RenderStats) {
        let mut sink: Vec<u8> = Vec::new();
        let stats = render_cast(header, events, &mut sink, format, opts).expect("render succeeds");
        (sink, stats)
    }

    #[test]
    fn fps_is_normalized_to_a_period_dividing_one_thousand() {
        for fps in 1..=50_u8 {
            let period = normalize_period_ms(fps);
            assert_eq!(1000 % period, 0, "fps {fps} gave period {period}");
        }
        assert_eq!(normalize_period_ms(10), 100);
        assert_eq!(normalize_period_ms(12), 100);
        assert_eq!(normalize_period_ms(24), 40);
        assert_eq!(normalize_period_ms(50), 20);
        assert_eq!(normalize_period_ms(1), 200);
    }

    #[test]
    fn default_options_match_the_documented_cli_defaults() {
        let opts = RenderOptions::default();
        assert_eq!(opts.fps, 10);
        assert_eq!(opts.idle_limit, Some(2.0));
        assert_eq!(opts.max_bytes, 8 * 1024 * 1024);
        assert_eq!(opts.tail_hold_ms, 2000);
        assert!(opts.theme.is_none());
    }

    #[test]
    fn two_output_events_render_at_least_two_frames() {
        let head = header(20, 4);
        let events = vec![output(0, "hello"), output(500, "\r\nworld")];
        let frames = frames_of(&head, &events);
        assert!(frames.len() >= 2, "got {} frames", frames.len());
    }

    #[test]
    fn clean_samples_extend_the_previous_delay_instead_of_emitting_a_frame() {
        let head = header(20, 4);
        // One paint, then a full second of nothing, then one more paint. At a
        // 100 ms period the quiet second is ten clean samples.
        let events = vec![output(0, "a"), output(1000, "b")];
        let frames = frames_of(&head, &events);
        assert_eq!(frames.len(), 2, "idle time must not cost frames");
        assert!(
            frames[0].1 >= 900,
            "the first frame should hold for the idle gap, got {} ms",
            frames[0].1
        );
    }

    #[test]
    fn idle_gap_longer_than_the_limit_is_collapsed() {
        let head = header(20, 4);
        let events = vec![output(0, "a"), output(30_000, "b")];
        let mut clamped = events.clone();
        clamp_idle(&mut clamped, Some(2.0));
        assert_eq!(clamped.last().expect("two events").time_ms, 2000);
        let long = frames_of(&head, &events);
        let short = frames_of(&head, &clamped);
        assert!(
            long.len() >= short.len(),
            "clamping must not make the walk longer"
        );
        // And the clamp is what `render_cast` applies before sampling.
        let (_bytes, stats) = render_to_vec(
            &head,
            &events,
            OutputFormat::Apng,
            &RenderOptions::default(),
        );
        assert_eq!(stats.duration_ms, 2000);
    }

    #[test]
    fn both_passes_produce_the_same_frame_count() {
        let head = header(20, 4);
        let events = vec![
            output(0, "one"),
            output(150, "\r\ntwo"),
            output(600, "\r\nthree"),
            output(1400, "\r\nfour"),
        ];
        let first = frames_of(&head, &events).len();
        let second = frames_of(&head, &events).len();
        assert_eq!(first, second, "the walk must be deterministic");

        let opts = RenderOptions {
            tail_hold_ms: 0,
            ..RenderOptions::default()
        };
        let (_bytes, stats) = render_to_vec(&head, &events, OutputFormat::Apng, &opts);
        assert_eq!(
            stats.frames as usize, first,
            "pass 2 emitted a different count than the walk pass 1 counted"
        );
    }

    #[test]
    fn resize_event_grows_the_canvas_and_letterboxes_smaller_frames() {
        let head = header(10, 3);
        let events = vec![output(0, "small"), resize(300, 20, 5), output(400, "big")];
        let opts = RenderOptions::default();
        let (bytes, stats) = render_to_vec(&head, &events, OutputFormat::Apng, &opts);
        // IHDR is the first chunk: 8 signature bytes, 4 length, 4 type, then
        // width and height as big-endian u32s.
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        assert_eq!(width, 20 * 8, "canvas must take the largest columns seen");
        assert_eq!(height, 5 * 16, "canvas must take the largest rows seen");
        assert!(stats.frames > 0);
    }

    #[test]
    fn keyframe_is_emitted_every_hundred_frames() {
        let surface = Surface {
            width: 64,
            height: 320,
            pixels: vec![[0, 0, 0]; 64 * 320],
        };
        let sampled = Sampled {
            frame: RenderedFrame::blank(8, 20),
            dirty_rows: Some((3, 3)),
        };
        // Frame 0 and frame 100 are keyframes; frame 1 honours the band.
        assert_eq!(frame_rect(&sampled, &surface, 16, 0), Rect::whole(&surface));
        assert_eq!(
            frame_rect(&sampled, &surface, 16, 100),
            Rect::whole(&surface)
        );
        assert_eq!(
            frame_rect(&sampled, &surface, 16, 1),
            Rect {
                x: 0,
                y: 48,
                w: 64,
                h: 16
            }
        );
    }

    #[test]
    fn max_bytes_stops_encoding_and_reports_truncated() {
        let head = header(80, 24);
        let mut events = Vec::new();
        for i in 0..40_u64 {
            events.push(output(i * 100, &format!("line {i}\r\n")));
        }
        let opts = RenderOptions {
            max_bytes: 512,
            tail_hold_ms: 0,
            ..RenderOptions::default()
        };
        let (bytes, stats) = render_to_vec(&head, &events, OutputFormat::Apng, &opts);
        assert!(stats.truncated, "a 512-byte cap must truncate this render");
        assert!(!bytes.is_empty(), "the container must still be closed");
        assert_eq!(bytes.get(..4), Some(&[0x89, b'P', b'N', b'G'][..]));
    }

    #[test]
    fn gif_render_has_the_gif89a_signature_and_a_plausible_size() {
        let head = header(40, 10);
        let events = vec![
            output(0, "\x1b[32mgreen\x1b[0m"),
            output(200, "\r\n\x1b[1;31mred\x1b[0m"),
            output(700, "\r\ndone"),
        ];
        let (bytes, stats) =
            render_to_vec(&head, &events, OutputFormat::Gif, &RenderOptions::default());
        assert_eq!(bytes.get(..6), Some(&b"GIF89a"[..]));
        assert_eq!(bytes.last(), Some(&0x3b));
        assert!(stats.frames >= 3, "got {} frames", stats.frames);
        assert!(!stats.truncated);
        assert!(bytes.len() > 100, "suspiciously small GIF");
        assert!(
            stats.colors > 1,
            "an SGR-colored recording must see more than one color"
        );
    }

    #[test]
    fn apng_render_has_the_png_signature_and_an_actl_chunk() {
        let head = header(40, 10);
        let events = vec![output(0, "hello"), output(300, "\r\nworld")];
        let (bytes, stats) = render_to_vec(
            &head,
            &events,
            OutputFormat::Apng,
            &RenderOptions::default(),
        );
        assert_eq!(
            bytes.get(..8),
            Some(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..])
        );
        assert!(
            bytes.windows(4).any(|window| window == b"acTL"),
            "an APNG without acTL is a still image"
        );
        assert!(stats.frames >= 2);
        assert!(!stats.truncated);
    }

    #[test]
    fn cast_to_cast_transcodes_v2_to_v3_and_back() {
        let head = CastHeader {
            cols: 80,
            rows: 24,
            title: Some("round trip".to_owned()),
            ..CastHeader::default()
        };
        let events = vec![
            output(0, "hello"),
            resize(400, 100, 30),
            output(900, "world"),
            CastEvent {
                time_ms: 1500,
                code: EventCode::Exit,
                data: "0".to_owned(),
            },
        ];
        // v2 comes out of `render_cast`; it is the documented default.
        let (v2_bytes, stats) = render_to_vec(
            &head,
            &events,
            OutputFormat::Cast,
            &RenderOptions::default(),
        );
        assert_eq!(stats.frames, 4);
        assert_eq!(stats.bytes, v2_bytes.len() as u64);
        let (v2_header, v2_events) =
            read_cast(BufReader::new(v2_bytes.as_slice())).expect("v2 parses");
        assert_eq!(v2_header.cols, 80);
        assert_eq!(v2_events, events);

        // v3 is the explicit opt-in, and the absolute timeline must survive
        // the trip through relative intervals unchanged.
        let mut v3_bytes: Vec<u8> = Vec::new();
        {
            let mut writer =
                CastWriter::new(&mut v3_bytes, &v2_header, CastVersion::V3).expect("v3 opens");
            for event in &v2_events {
                let at = Duration::from_millis(event.time_ms);
                match event.code {
                    EventCode::Output => writer.output(at, event.data.as_bytes()),
                    EventCode::Resize => {
                        let (cols, rows) = parse_resize(&event.data).expect("resize parses");
                        writer.resize(at, cols, rows)
                    }
                    EventCode::Exit => writer.exit(at, 0),
                    EventCode::Marker => writer.marker(at, &event.data),
                    EventCode::Input => Ok(()),
                }
                .expect("event writes");
            }
            writer.finish().expect("v3 finishes");
        }
        let (v3_header, v3_events) =
            read_cast(BufReader::new(v3_bytes.as_slice())).expect("v3 parses");
        assert_eq!(v3_header.cols, 80);
        assert_eq!(v3_events, events);
    }

    #[test]
    fn an_empty_event_list_still_produces_one_frame() {
        // The replayer's first sample never reports clean, so even a recording
        // with nothing in it exports an opening frame rather than a
        // zero-frame container that no viewer will open.
        let head = header(20, 4);
        let (bytes, stats) =
            render_to_vec(&head, &[], OutputFormat::Gif, &RenderOptions::default());
        assert_eq!(stats.frames, 1);
        assert_eq!(bytes.get(..6), Some(&b"GIF89a"[..]));
    }

    #[test]
    fn a_malformed_resize_event_is_skipped_not_fatal() {
        let head = header(20, 4);
        let events = vec![
            output(0, "a"),
            CastEvent {
                time_ms: 200,
                code: EventCode::Resize,
                data: "not-a-size".to_owned(),
            },
            output(400, "b"),
        ];
        let (_bytes, stats) = render_to_vec(
            &head,
            &events,
            OutputFormat::Apng,
            &RenderOptions::default(),
        );
        assert!(stats.frames >= 2);
    }
}
