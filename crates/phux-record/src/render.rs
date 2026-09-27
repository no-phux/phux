//! The two-pass render driver: cast events in, animation out.
//!
//! [`render_cast`] replays the events twice. Pass 1 collects the colour set
//! for GIF's global table and the frame count APNG's `acTL` needs up front;
//! pass 2 replays from scratch and encodes. Both passes drive one
//! `SampleWalk` over deterministic replay, so they agree on frame count by
//! construction, and memory stays O(one surface).
//!
//! Sampling is fixed-period (`1000 / fps` ms, normalized so the period divides
//! 1000 exactly). A clean sample emits no frame; its period folds into the
//! previous frame's delay, so idle time is free. A frame's delay is only
//! known when the next one arrives, so the walk holds one frame back.

use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use crate::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode};
use crate::encode::{AnimEncoder, CountingWriter, Palette, Rect};
use crate::error::RecordError;
use crate::raster::{Rasterizer, Surface};
use crate::replay::{Replayer, Sampled};
use crate::timeline::clamp_idle;

/// Longest single accumulated delay, in milliseconds (GIF caps at 500 cs).
const MAX_DELAY_MS: u32 = 5_000;

/// Force a whole-canvas frame this often, bounding how stale a `--max-bytes`
/// truncated file can get.
const KEYFRAME_INTERVAL: u32 = 100;

/// What a recording is exported as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Re-serialize as an asciicast (v2).
    Cast,
    /// Animated `GIF89a`: the shareable default.
    #[default]
    Gif,
    /// Animated PNG: truecolor, 1 ms timing resolution.
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
    /// many bytes.
    pub max_bytes: u64,
    /// Hold the final frame this long before the loop wraps.
    pub tail_hold_ms: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            fps: 10,
            idle_limit: Some(2.0),
            max_bytes: 8 * 1024 * 1024,
            tail_hold_ms: 2000,
        }
    }
}

/// What a render produced, for the CLI's one-liner and `--json` object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Frames (or, for cast output, events) emitted.
    pub frames: u32,
    /// Bytes written to the sink.
    pub bytes: u64,
    /// Duration of the rendered timeline, after idle clamping.
    pub duration_ms: u64,
    /// Distinct colours seen; above 256 the GIF palette is nearest-fit.
    pub colors: u32,
    /// Whether `max_bytes` cut the render short.
    pub truncated: bool,
}

/// Render `events` into `sink` as `format`.
///
/// A `max_bytes` overrun is not an error: the container is closed cleanly and
/// [`RenderStats::truncated`] is set.
pub fn render_cast<W: Write>(
    header: &CastHeader,
    events: &[CastEvent],
    sink: W,
    format: OutputFormat,
    opts: &RenderOptions,
) -> Result<RenderStats, RecordError> {
    let mut events = events.to_vec();
    clamp_idle(&mut events, opts.idle_limit);
    let duration_ms = events.last().map_or(0, |event| event.time_ms);

    if format == OutputFormat::Cast {
        return transcode(header, &events, sink, duration_ms);
    }
    let pass1 = survey(header, &events, opts)?;
    encode(header, &events, sink, format, opts, &pass1, duration_ms)
}

/// Re-serialize as asciicast v2, the version every consumer reads
/// (ADR-0060). Input events are never re-emitted.
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
            // A non-numeric status is not worth failing a transcode over.
            EventCode::Exit => writer.exit(at, event.data.trim().parse::<i32>().unwrap_or(0))?,
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
struct Survey {
    frames: u32,
    colors: HashSet<[u8; 3]>,
    rasterizer: Rasterizer,
    cols: u16,
    rows: u16,
}

/// Pass 1: count frames, collect colours, and find the largest grid.
fn survey(
    header: &CastHeader,
    events: &[CastEvent],
    opts: &RenderOptions,
) -> Result<Survey, RecordError> {
    let mut walk = SampleWalk::new(header, events, opts)?;
    let mut colors = HashSet::new();
    let mut frames = 0_u32;
    let mut rasterizer: Option<Rasterizer> = None;

    while let Some((sampled, _delay)) = walk.next_frame()? {
        // The theme is read right after the first sample, once it has settled.
        let raster = match rasterizer {
            Some(ref existing) => existing,
            None => rasterizer.insert(Rasterizer::new(walk.replayer.theme()?)),
        };
        raster.colors_of(&sampled.frame, &mut colors);
        frames = frames.saturating_add(1);
    }

    let rasterizer = rasterizer.unwrap_or_else(|| Rasterizer::new(crate::raster::Theme::default()));
    // Even an empty recording needs a background for the canvas.
    colors.insert(rasterizer.theme().bg);
    Ok(Survey {
        frames,
        colors,
        rasterizer,
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
    let rasterizer = &pass1.rasterizer;
    let mut surface = rasterizer.surface_for(pass1.cols, pass1.rows);
    let (_cell_w, cell_h) = rasterizer.cell_size();

    let mut encoder = if format == OutputFormat::Apng {
        AnimEncoder::apng(sink, surface.width, surface.height, pass1.frames.max(1))?
    } else {
        let palette = Palette::build(&pass1.colors, rasterizer.theme());
        AnimEncoder::gif(sink, surface.width, surface.height, palette)?
    };

    let mut walk = SampleWalk::new(header, events, opts)?;
    let mut frames = 0_u32;
    let mut truncated = false;

    while let Some((sampled, delay_ms)) = walk.next_frame()? {
        rasterizer.draw(&sampled.frame, &mut surface);
        let rect = frame_rect(&sampled, &surface, cell_h, frames);
        let bytes = encoder.add_frame(&surface, rect, delay_ms)?;
        frames = frames.saturating_add(1);
        if bytes >= opts.max_bytes {
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

/// The rectangle a sampled frame is encoded as: the dirty band of full-width
/// rows, or the whole canvas on keyframes and whole-canvas samples.
fn frame_rect(sampled: &Sampled, surface: &Surface, cell_h: u32, emitted: u32) -> Rect {
    match sampled.dirty_rows {
        Some((first, last)) if !emitted.is_multiple_of(KEYFRAME_INTERVAL) => {
            let span = u32::from(last.saturating_sub(first).saturating_add(1));
            Rect {
                x: 0,
                y: u32::from(first).saturating_mul(cell_h),
                w: surface.width,
                h: span.saturating_mul(cell_h),
            }
        }
        _ => Rect::whole(surface),
    }
}

/// Drives the replayer on the export's fixed sample clock.
struct SampleWalk<'a> {
    events: &'a [CastEvent],
    replayer: Replayer,
    /// Index of the next event to feed.
    idx: usize,
    /// The sample instant, on the cast's millisecond timeline.
    next_ms: u64,
    period_ms: u32,
    tail_hold_ms: u32,
    /// The frame whose delay is still accumulating.
    pending: Option<Sampled>,
    pending_delay_ms: u32,
    /// Set once the event list is exhausted; the next call flushes `pending`.
    done: bool,
    /// Largest grid seen, so the canvas never grows mid-animation.
    max_cols: u16,
    max_rows: u16,
}

impl<'a> SampleWalk<'a> {
    fn new(
        header: &CastHeader,
        events: &'a [CastEvent],
        opts: &RenderOptions,
    ) -> Result<Self, RecordError> {
        // A zero-dimension header is malformed; 80x24 is the universal
        // fallback and the recording's own resize events will correct it.
        let cols = if header.cols == 0 { 80 } else { header.cols };
        let rows = if header.rows == 0 { 24 } else { header.rows };
        Ok(Self {
            events,
            replayer: Replayer::new(cols, rows)?,
            idx: 0,
            next_ms: 0,
            period_ms: normalize_period_ms(opts.fps),
            tail_hold_ms: opts.tail_hold_ms,
            pending: None,
            pending_delay_ms: 0,
            done: false,
            max_cols: cols,
            max_rows: rows,
        })
    }

    /// The next frame whose delay is now known, or `None` at the end.
    fn next_frame(&mut self) -> Result<Option<(Sampled, u32)>, RecordError> {
        loop {
            if self.done {
                let hold = self
                    .pending_delay_ms
                    .min(MAX_DELAY_MS)
                    .saturating_add(self.tail_hold_ms);
                return Ok(self.pending.take().map(|sampled| (sampled, hold)));
            }

            self.feed_due_events()?;
            let ready = if let Some(sampled) = self.replayer.sample()? {
                self.max_cols = self.max_cols.max(sampled.frame.cols);
                self.max_rows = self.max_rows.max(sampled.frame.rows);
                let previous = self.pending.replace(sampled);
                let delay = self.pending_delay_ms.min(MAX_DELAY_MS);
                self.pending_delay_ms = self.period_ms;
                previous.map(|frame| (frame, delay))
            } else {
                self.pending_delay_ms = self.pending_delay_ms.saturating_add(self.period_ms);
                None
            };

            if self.idx >= self.events.len() {
                self.done = true;
            } else {
                self.next_ms = self.next_ms.saturating_add(u64::from(self.period_ms));
            }
            if ready.is_some() {
                return Ok(ready);
            }
        }
    }

    /// Feed every event at or before the current sample instant. Malformed
    /// resizes are skipped rather than fatal.
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

/// Normalize an fps to the nearest of `{5, 10, 20, 25, 50}`, whose periods
/// divide 1000 ms exactly: no clock drift, exact GIF centisecond delays.
fn normalize_period_ms(fps: u8) -> u32 {
    const ALLOWED: [(u8, u32); 5] = [(5, 200), (10, 100), (20, 50), (25, 40), (50, 20)];
    let target = fps.max(1);
    ALLOWED
        .iter()
        .min_by_key(|(candidate, _)| candidate.abs_diff(target))
        .map_or(100, |(_, period)| *period)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
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

    fn event(time_ms: u64, code: EventCode, data: &str) -> CastEvent {
        CastEvent {
            time_ms,
            code,
            data: data.to_owned(),
        }
    }

    fn output(time_ms: u64, data: &str) -> CastEvent {
        event(time_ms, EventCode::Output, data)
    }

    fn frames_of(header: &CastHeader, events: &[CastEvent]) -> Vec<(Sampled, u32)> {
        let opts = RenderOptions {
            tail_hold_ms: 0,
            ..RenderOptions::default()
        };
        let mut walk = SampleWalk::new(header, events, &opts).expect("walk builds");
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
        for fps in 0..=u8::MAX {
            let period = normalize_period_ms(fps);
            assert_eq!(1000 % period, 0, "fps {fps} gave period {period}");
        }
        for (fps, period) in [(1, 200), (10, 100), (12, 100), (24, 40), (50, 20)] {
            assert_eq!(normalize_period_ms(fps), period, "fps {fps}");
        }
    }

    #[test]
    fn clean_samples_extend_the_previous_delay_instead_of_emitting_a_frame() {
        // At a 100 ms period the quiet second is ten clean samples.
        let frames = frames_of(&header(20, 4), &[output(0, "a"), output(1000, "b")]);
        assert_eq!(frames.len(), 2, "idle time must not cost frames");
        assert!(frames[0].1 >= 900, "first frame held {} ms", frames[0].1);
    }

    #[test]
    fn apng_and_gif_agree_with_the_walk_and_clamp_idle() {
        let head = header(40, 10);
        let events = vec![
            output(0, "\x1b[32mgreen\x1b[0m"),
            output(200, "\r\n\x1b[1;31mred\x1b[0m"),
            output(30_000, "\r\ndone"),
        ];
        let mut clamped = events.clone();
        clamp_idle(&mut clamped, Some(2.0));
        let walked = frames_of(&head, &clamped).len();
        let opts = RenderOptions {
            tail_hold_ms: 0,
            ..RenderOptions::default()
        };

        let (apng, apng_stats) = render_to_vec(&head, &events, OutputFormat::Apng, &opts);
        assert_eq!(
            apng.get(..8),
            Some(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..])
        );
        assert!(apng.windows(4).any(|window| window == b"acTL"));
        assert_eq!(apng_stats.frames as usize, walked, "pass 2 != pass 1 count");
        assert_eq!(apng_stats.duration_ms, 2200, "the idle clamp applies");

        let (gif, gif_stats) = render_to_vec(&head, &events, OutputFormat::Gif, &opts);
        assert_eq!(gif.get(..6), Some(&b"GIF89a"[..]));
        assert_eq!(gif.last(), Some(&0x3b));
        assert_eq!(gif_stats.frames, apng_stats.frames);
        assert!(gif_stats.colors > 2, "SGR colours must reach the palette");
        assert!(!gif_stats.truncated && !apng_stats.truncated);
    }

    #[test]
    fn resize_event_grows_the_canvas_and_malformed_resizes_are_skipped() {
        let events = vec![
            output(0, "small"),
            event(100, EventCode::Resize, "not-a-size"),
            event(300, EventCode::Resize, "20x5"),
            output(400, "big"),
        ];
        let (bytes, stats) = render_to_vec(
            &header(10, 3),
            &events,
            OutputFormat::Apng,
            &RenderOptions::default(),
        );
        // IHDR width and height follow the 8-byte signature and chunk header.
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        assert_eq!((width, height), (20 * 8, 5 * 16));
        assert!(stats.frames >= 2);
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
        assert_eq!(frame_rect(&sampled, &surface, 16, 0), Rect::whole(&surface));
        assert_eq!(
            frame_rect(&sampled, &surface, 16, 100),
            Rect::whole(&surface)
        );
        let band = Rect {
            x: 0,
            y: 48,
            w: 64,
            h: 16,
        };
        assert_eq!(frame_rect(&sampled, &surface, 16, 1), band);
    }

    #[test]
    fn max_bytes_stops_encoding_and_reports_truncated() {
        let events: Vec<CastEvent> = (0..40_u64)
            .map(|i| output(i * 100, &format!("line {i}\r\n")))
            .collect();
        let opts = RenderOptions {
            max_bytes: 512,
            tail_hold_ms: 0,
            ..RenderOptions::default()
        };
        let (bytes, stats) = render_to_vec(&header(80, 24), &events, OutputFormat::Apng, &opts);
        assert!(stats.truncated, "a 512-byte cap must truncate this render");
        assert!(
            bytes.windows(4).any(|window| window == b"IEND"),
            "not closed"
        );
    }

    #[test]
    fn an_empty_event_list_still_produces_one_frame() {
        let (bytes, stats) = render_to_vec(
            &header(20, 4),
            &[],
            OutputFormat::Gif,
            &RenderOptions::default(),
        );
        assert_eq!(stats.frames, 1);
        assert_eq!(bytes.get(..6), Some(&b"GIF89a"[..]));
    }

    #[test]
    fn cast_output_transcodes_to_v2_and_drops_input() {
        let head = CastHeader {
            title: Some("round trip".to_owned()),
            ..header(80, 24)
        };
        let events = vec![
            output(0, "hello"),
            event(100, EventCode::Input, "secret"),
            event(400, EventCode::Resize, "100x30"),
            event(600, EventCode::Marker, "chapter"),
            output(900, "world"),
            event(1500, EventCode::Exit, "0"),
        ];
        let (bytes, stats) = render_to_vec(
            &head,
            &events,
            OutputFormat::Cast,
            &RenderOptions::default(),
        );
        assert_eq!(stats.frames, 5);
        assert_eq!(stats.bytes, bytes.len() as u64);
        assert!(bytes.starts_with(br#"{"version":2,"#));
        let (parsed_header, parsed) = read_cast(bytes.as_slice()).expect("v2 parses");
        assert_eq!(parsed_header, head);
        let mut want = events;
        want.remove(1);
        assert_eq!(parsed, want);
    }
}
