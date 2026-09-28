//! asciicast v2 / v3 reader and writer.
//!
//! NDJSON: a JSON object header, then one `[time, "code", "data"]` array per
//! event. v2 puts `width`/`height` on the header and writes absolute times;
//! v3 nests `term.cols`/`term.rows` (and `theme`) and writes intervals since
//! the previous event. A v2-only reader misreads v3 intervals as absolute
//! times, so v2 is the default written version (ADR-0060).
//!
//! [`CastEvent::time_ms`] is absolute integer milliseconds in both directions,
//! formatted as `{secs}.{millis:03}` with integer arithmetic, so the `.cast`
//! and the GIF rendered from it cannot drift apart. Written times are clamped
//! monotonic. [`EventCode::Input`] exists only so foreign recordings
//! round-trip; phux never records input.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use crate::error::RecordError;

/// Which asciicast revision to serialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CastVersion {
    /// Flat header dims, absolute event times. Read by every asciinema tool.
    #[default]
    V2,
    /// `term`-nested header dims, relative event intervals.
    V3,
}

impl CastVersion {
    /// The integer written into the header's `version` key.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }
}

/// The single-character event code of an asciicast event line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCode {
    /// `o` — terminal output. The overwhelming majority of every recording.
    Output,
    /// `i` — terminal input. Read-only for phux; see the module docs.
    Input,
    /// `m` — a named marker.
    Marker,
    /// `r` — a resize; the data is `"{COLS}x{ROWS}"`.
    Resize,
    /// `x` — process exit; the data is the stringified exit status.
    Exit,
}

impl EventCode {
    /// The wire character for this code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Output => "o",
            Self::Input => "i",
            Self::Marker => "m",
            Self::Resize => "r",
            Self::Exit => "x",
        }
    }

    /// Parse a wire character, or `None` for an unknown code (which readers
    /// must tolerate: it is the format's only extension mechanism).
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "o" => Some(Self::Output),
            "i" => Some(Self::Input),
            "m" => Some(Self::Marker),
            "r" => Some(Self::Resize),
            "x" => Some(Self::Exit),
            _ => None,
        }
    }
}

/// The terminal color theme recorded in a cast header.
///
/// `palette` carries 8 or 16 entries; anything else is rejected on read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastTheme {
    /// Default foreground.
    pub fg: [u8; 3],
    /// Default background.
    pub bg: [u8; 3],
    /// 8 or 16 ANSI palette entries.
    pub palette: Vec<[u8; 3]>,
}

/// The asciicast header: everything known before the first event.
///
/// Optional fields are omitted when `None`, never written as `null`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CastHeader {
    /// Initial terminal width in columns.
    pub cols: u16,
    /// Initial terminal height in rows.
    pub rows: u16,
    /// Unix timestamp of the recording's start.
    pub timestamp: Option<u64>,
    /// The idle clamp that was applied, in seconds, for players to display.
    pub idle_time_limit: Option<f64>,
    /// The command that was recorded.
    pub command: Option<String>,
    /// A human title for the recording.
    pub title: Option<String>,
    /// Captured environment (conventionally just `TERM` and `SHELL`).
    pub env: BTreeMap<String, String>,
    /// The terminal theme in effect.
    pub theme: Option<CastTheme>,
}

/// One asciicast event, on the crate's absolute-millisecond timebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastEvent {
    /// Milliseconds since session start, absolute in both v2 and v3.
    pub time_ms: u64,
    /// What kind of event this is.
    pub code: EventCode,
    /// The event payload, already unescaped.
    pub data: String,
}

/// A streaming asciicast writer.
///
/// The header goes out in [`CastWriter::new`] and the sink is flushed after
/// every event, so a crashed session leaves a playable prefix.
///
/// PTY reads can split a multi-byte character, so [`CastWriter::output`]
/// holds back an incomplete trailing UTF-8 sequence for the next call rather
/// than splicing U+FFFD at every chunk boundary.
pub struct CastWriter<W: Write> {
    sink: W,
    version: CastVersion,
    /// Absolute milliseconds of the last emitted event; the monotonic floor,
    /// and the base v3 intervals are measured from.
    last_ms: u64,
    /// Bytes held back because they are an incomplete UTF-8 sequence.
    tail: Vec<u8>,
}

impl<W: Write> std::fmt::Debug for CastWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastWriter")
            .field("version", &self.version)
            .field("last_ms", &self.last_ms)
            .field("pending_tail_bytes", &self.tail.len())
            .finish_non_exhaustive()
    }
}

impl<W: Write> CastWriter<W> {
    /// Open a writer, emitting the header line immediately.
    pub fn new(sink: W, header: &CastHeader, version: CastVersion) -> Result<Self, RecordError> {
        let mut this = Self {
            sink,
            version,
            last_ms: 0,
            tail: Vec::new(),
        };
        let line = serialize_header(header, version);
        this.sink.write_all(line.as_bytes())?;
        this.sink.write_all(b"\n")?;
        this.sink.flush()?;
        Ok(this)
    }

    /// Timestamp of the last emitted event, for callers that rewrite the
    /// header's `duration` key once the recording ends.
    #[must_use]
    pub const fn elapsed_ms(&self) -> u64 {
        self.last_ms
    }

    /// Record terminal output captured at `at` after session start.
    ///
    /// Emits at most one event: an incomplete trailing UTF-8 sequence is held
    /// back, and a chunk that decodes to nothing emits nothing at all.
    pub fn output(&mut self, at: Duration, bytes: &[u8]) -> Result<(), RecordError> {
        self.tail.extend_from_slice(bytes);
        let mut text = String::new();
        drain_utf8(&mut self.tail, &mut text);
        if text.is_empty() {
            return Ok(());
        }
        self.emit(duration_ms(at), EventCode::Output, &text)
    }

    /// Record a terminal resize to `cols` x `rows`.
    pub fn resize(&mut self, at: Duration, cols: u16, rows: u16) -> Result<(), RecordError> {
        self.emit(
            duration_ms(at),
            EventCode::Resize,
            &format!("{cols}x{rows}"),
        )
    }

    /// Record a named marker.
    pub fn marker(&mut self, at: Duration, label: &str) -> Result<(), RecordError> {
        self.emit(duration_ms(at), EventCode::Marker, label)
    }

    /// Record process exit with `status`.
    pub fn exit(&mut self, at: Duration, status: i32) -> Result<(), RecordError> {
        self.emit(duration_ms(at), EventCode::Exit, &status.to_string())
    }

    /// Flush any residual UTF-8 tail and return the sink.
    ///
    /// A tail still present here is genuinely truncated and is emitted as one
    /// U+FFFD rather than silently dropped.
    pub fn finish(mut self) -> Result<W, RecordError> {
        if !self.tail.is_empty() {
            self.tail.clear();
            let at = self.last_ms;
            self.emit(at, EventCode::Output, "\u{fffd}")?;
        }
        self.sink.flush()?;
        Ok(self.sink)
    }

    /// Serialize one event line and flush.
    fn emit(&mut self, at_ms: u64, code: EventCode, data: &str) -> Result<(), RecordError> {
        // A backwards clock must not produce a negative v3 interval.
        let ms = at_ms.max(self.last_ms);
        let stamp = match self.version {
            CastVersion::V2 => format_secs(ms),
            CastVersion::V3 => format_secs(ms - self.last_ms),
        };
        self.last_ms = ms;
        let line = format!("[{stamp}, \"{}\", {}]\n", code.as_str(), json_str(data));
        self.sink.write_all(line.as_bytes())?;
        self.sink.flush()?;
        Ok(())
    }
}

/// Read a v2 or v3 asciicast, normalizing both onto absolute milliseconds.
///
/// Unknown event codes are skipped (their v3 interval still advances the
/// clock). Blank and `#` comment lines are ignored. asciicast v1 is rejected.
pub fn read_cast<R: BufRead>(src: R) -> Result<(CastHeader, Vec<CastEvent>), RecordError> {
    let mut lines = src.lines();
    let header_line = read_header_line(&mut lines)?;
    let (header, version) = parse_header_line(&header_line)?;
    let events = read_events(lines, version == CastVersion::V3)?;
    Ok((header, events))
}

/// The first non-blank line, which is where the header must be.
fn read_header_line<R: BufRead>(lines: &mut std::io::Lines<R>) -> Result<String, RecordError> {
    for line in lines {
        let line = line?;
        if !line.trim().is_empty() {
            return Ok(line);
        }
    }
    Err(RecordError::Cast("input is empty".to_owned()))
}

/// Parse the header line, dispatching on its declared `version`.
fn parse_header_line(line: &str) -> Result<(CastHeader, CastVersion), RecordError> {
    let raw: serde_json::Value = serde_json::from_str(line)
        .map_err(|err| RecordError::Cast(format!("header is not JSON: {err}")))?;
    let version = raw
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| RecordError::Cast("header has no numeric `version`".to_owned()))?;

    match version {
        2 => Ok((
            parse_header(&raw, &raw, "", "width", "height")?,
            CastVersion::V2,
        )),
        3 => {
            let term = raw
                .get("term")
                .ok_or_else(|| RecordError::Cast("v3 header has no `term` object".to_owned()))?;
            let header = parse_header(&raw, term, "term.", "cols", "rows")?;
            Ok((header, CastVersion::V3))
        }
        1 => Err(RecordError::Cast(
            "asciicast v1 is not supported; re-record or convert with `asciinema convert`"
                .to_owned(),
        )),
        other => Err(RecordError::Cast(format!(
            "unknown asciicast version {other}; this build reads v2 and v3"
        ))),
    }
}

/// Read every event line onto the absolute-millisecond timebase; `relative`
/// says whether stamps are v3 intervals.
fn read_events<R: BufRead>(
    lines: std::io::Lines<R>,
    relative: bool,
) -> Result<Vec<CastEvent>, RecordError> {
    let mut events = Vec::new();
    let mut clock = 0_u64;
    for line in lines {
        let line = line?;
        let trimmed = line.trim();
        // v3 permits `#` comments; tolerating them in v2 costs nothing.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let event = parse_event_line(trimmed)?;
        // Advance before the skip so an unknown code cannot shift later v3
        // events. v2 stamps are clamped monotonic.
        clock = if relative {
            clock.saturating_add(event.ms)
        } else {
            event.ms.max(clock)
        };
        let Some(code) = EventCode::from_code(&event.code) else {
            continue;
        };
        events.push(CastEvent {
            time_ms: clock,
            code,
            data: event.data,
        });
    }

    Ok(events)
}

/// One event line's fields, before the code is resolved or the clock folded
/// in. `ms` is absolute in v2 and an interval in v3.
#[derive(Debug)]
struct RawEvent {
    ms: u64,
    code: String,
    data: String,
}

/// Split one event line into its three JSON array slots.
fn parse_event_line(line: &str) -> Result<RawEvent, RecordError> {
    let parsed: serde_json::Value = serde_json::from_str(line)
        .map_err(|err| RecordError::Cast(format!("event line is not JSON: {err}")))?;
    let items = parsed
        .as_array()
        .ok_or_else(|| RecordError::Cast("event line is not a JSON array".to_owned()))?;
    let secs = items
        .first()
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| RecordError::Cast("event line has no numeric time".to_owned()))?;
    let code = items
        .get(1)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| RecordError::Cast("event line has no string code".to_owned()))?
        .to_owned();
    let data = items
        .get(2)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();

    Ok(RawEvent {
        ms: secs_to_ms(secs),
        code,
        data,
    })
}

/// Parse the fields shared by both header versions. `dims` holds the size
/// and theme keys (the header itself in v2, `term` in v3).
fn parse_header(
    raw: &serde_json::Value,
    dims: &serde_json::Value,
    prefix: &str,
    cols_key: &str,
    rows_key: &str,
) -> Result<CastHeader, RecordError> {
    Ok(CastHeader {
        cols: dim(dims.get(cols_key), &format!("{prefix}{cols_key}"))?,
        rows: dim(dims.get(rows_key), &format!("{prefix}{rows_key}"))?,
        timestamp: raw.get("timestamp").and_then(serde_json::Value::as_u64),
        idle_time_limit: raw
            .get("idle_time_limit")
            .and_then(serde_json::Value::as_f64),
        command: opt_string(raw.get("command")),
        title: opt_string(raw.get("title")),
        env: parse_env(raw.get("env")),
        theme: parse_theme(dims.get("theme")),
    })
}

fn dim(value: Option<&serde_json::Value>, name: &str) -> Result<u16, RecordError> {
    let n = value
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| RecordError::Cast(format!("header has no numeric `{name}`")))?;
    u16::try_from(n).map_err(|_| RecordError::Cast(format!("header `{name}` = {n} exceeds u16")))
}

fn opt_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn parse_env(value: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(obj) = value.and_then(serde_json::Value::as_object) {
        for (key, item) in obj {
            if let Some(text) = item.as_str() {
                out.insert(key.clone(), text.to_owned());
            }
        }
    }
    out
}

fn parse_theme(value: Option<&serde_json::Value>) -> Option<CastTheme> {
    let obj = value?;
    let fg = parse_hex(obj.get("fg")?.as_str()?)?;
    let bg = parse_hex(obj.get("bg")?.as_str()?)?;
    let raw = obj.get("palette")?;
    // The spec shape is a colon-delimited string; accept arrays on read too.
    let palette: Vec<[u8; 3]> = match raw {
        serde_json::Value::String(text) => text.split(':').filter_map(parse_hex).collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| parse_hex(item.as_str()?))
            .collect(),
        _ => return None,
    };
    if palette.len() == 8 || palette.len() == 16 {
        Some(CastTheme { fg, bg, palette })
    } else {
        None
    }
}

/// Parse `#rrggbb` (or bare `rrggbb`) into a byte triple.
fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let body = text.strip_prefix('#').unwrap_or(text);
    if body.len() != 6 || !body.is_ascii() {
        return None;
    }
    let mut out = [0_u8; 3];
    for (slot, chunk) in out.iter_mut().zip(0..3) {
        let start = chunk * 2;
        let pair = body.get(start..start + 2)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn hex_of(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// Render whole milliseconds as fixed-3-decimal seconds without a float.
fn format_secs(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Whole milliseconds of a `Duration`, saturating rather than wrapping.
fn duration_ms(at: Duration) -> u64 {
    u64::try_from(at.as_millis()).unwrap_or(u64::MAX)
}

/// JSON-escape a string via `serde_json` (recorded data is full of C0 bytes).
fn json_str(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// Move every decodable character out of `tail` and into `out`, leaving only
/// an incomplete trailing sequence behind.
///
/// Invalid bytes become one U+FFFD each; only a prefix of a valid sequence is
/// retained for the next chunk.
fn drain_utf8(tail: &mut Vec<u8>, out: &mut String) {
    loop {
        match std::str::from_utf8(tail) {
            Ok(text) => {
                out.push_str(text);
                tail.clear();
                return;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if let Some(head) = tail.get(..valid)
                    && let Ok(text) = std::str::from_utf8(head)
                {
                    out.push_str(text);
                }
                if let Some(bad) = err.error_len() {
                    out.push(char::REPLACEMENT_CHARACTER);
                    tail.drain(..valid.saturating_add(bad));
                } else {
                    tail.drain(..valid);
                    return;
                }
            }
        }
    }
}

/// Insertion-ordered JSON object builder: `serde_json::Map` would sort the
/// header keys, and the spec documents an order.
struct JsonObject {
    buf: String,
    empty: bool,
}

impl JsonObject {
    fn new() -> Self {
        Self {
            buf: String::from("{"),
            empty: true,
        }
    }

    /// Append `"key": <already-serialized value>`.
    fn raw(&mut self, key: &str, value: &str) {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        self.buf.push_str(&json_str(key));
        self.buf.push(':');
        self.buf.push_str(value);
    }

    fn string(&mut self, key: &str, value: &str) {
        self.raw(key, &json_str(value));
    }

    fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

fn serialize_theme(theme: &CastTheme) -> String {
    let mut obj = JsonObject::new();
    obj.string("fg", &hex_of(theme.fg));
    obj.string("bg", &hex_of(theme.bg));
    // Colon-delimited, not an array, in both versions.
    let joined = theme
        .palette
        .iter()
        .map(|color| hex_of(*color))
        .collect::<Vec<_>>()
        .join(":");
    obj.string("palette", &joined);
    obj.finish()
}

fn serialize_env(env: &BTreeMap<String, String>) -> String {
    let mut obj = JsonObject::new();
    for (key, value) in env {
        obj.string(key, value);
    }
    obj.finish()
}

/// Append the shared optional tail (`timestamp`, `idle_time_limit`,
/// `command`, `title`, `env`) that both versions carry at top level.
fn push_common_optionals(obj: &mut JsonObject, header: &CastHeader) {
    if let Some(ts) = header.timestamp {
        obj.raw("timestamp", &ts.to_string());
    }
    if let Some(limit) = header.idle_time_limit
        && let Some(number) = serde_json::Number::from_f64(limit)
    {
        obj.raw("idle_time_limit", &number.to_string());
    }
    if let Some(command) = &header.command {
        obj.string("command", command);
    }
    if let Some(title) = &header.title {
        obj.string("title", title);
    }
    if !header.env.is_empty() {
        obj.raw("env", &serialize_env(&header.env));
    }
}

fn serialize_header(header: &CastHeader, version: CastVersion) -> String {
    let mut obj = JsonObject::new();
    obj.raw("version", &version.number().to_string());
    match version {
        CastVersion::V2 => {
            obj.raw("width", &header.cols.to_string());
            obj.raw("height", &header.rows.to_string());
            push_common_optionals(&mut obj, header);
            if let Some(theme) = &header.theme {
                obj.raw("theme", &serialize_theme(theme));
            }
        }
        CastVersion::V3 => {
            let mut term = JsonObject::new();
            term.raw("cols", &header.cols.to_string());
            term.raw("rows", &header.rows.to_string());
            if let Some(theme) = &header.theme {
                term.raw("theme", &serialize_theme(theme));
            }
            obj.raw("term", &term.finish());
            push_common_optionals(&mut obj, header);
        }
    }
    obj.finish()
}

/// Convert fractional seconds to whole milliseconds, saturating. NaN,
/// negatives, and overflow are handled before the cast.
pub(crate) fn secs_to_ms(secs: f64) -> u64 {
    if !secs.is_finite() || secs <= 0.0 {
        return 0;
    }
    let ms = (secs * 1000.0).round();
    // 2^63, exactly representable and below `u64::MAX`.
    if ms >= 9_223_372_036_854_775_808.0 {
        return u64::MAX;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the two guards above prove the value is finite, positive, and below 2^63"
    )]
    let whole = ms as u64;
    whole
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

    #[test]
    fn secs_to_ms_rejects_nonsense_without_panicking() {
        assert_eq!(secs_to_ms(f64::NAN), 0);
        assert_eq!(secs_to_ms(f64::NEG_INFINITY), 0);
        assert_eq!(secs_to_ms(-3.0), 0);
        assert_eq!(secs_to_ms(f64::INFINITY), 0);
        assert_eq!(secs_to_ms(2.0), 2000);
        assert_eq!(secs_to_ms(0.117), 117);
    }

    /// Drive a writer over an in-memory sink and hand back the lines.
    fn write_lines(
        version: CastVersion,
        header: &CastHeader,
        body: impl FnOnce(&mut CastWriter<Vec<u8>>),
    ) -> Vec<String> {
        let mut writer = CastWriter::new(Vec::new(), header, version).expect("writer opens");
        body(&mut writer);
        let bytes = writer.finish().expect("writer finishes");
        String::from_utf8(bytes)
            .expect("output is utf-8")
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }

    fn header_80x24() -> CastHeader {
        CastHeader {
            cols: 80,
            rows: 24,
            ..CastHeader::default()
        }
    }

    /// The literal time text of an event line: `0.100` and `0.1` are the same
    /// f64, so parsing would erase what these tests check.
    fn stamp_of(line: &str) -> String {
        let body = line.strip_prefix('[').expect("event line starts with [");
        let end = body.find(',').expect("event line has a comma");
        body.get(..end).expect("slice is in range").to_owned()
    }

    fn stamps(lines: &[String]) -> Vec<String> {
        lines.iter().skip(1).map(|line| stamp_of(line)).collect()
    }

    #[test]
    fn header_shape_per_version_omits_unset_keys() {
        let v2 = write_lines(CastVersion::V2, &header_80x24(), |_| {});
        assert_eq!(v2[0], r#"{"version":2,"width":80,"height":24}"#);
        let v3 = write_lines(CastVersion::V3, &header_80x24(), |_| {});
        assert_eq!(v3[0], r#"{"version":3,"term":{"cols":80,"rows":24}}"#);
    }

    #[test]
    fn v2_times_are_absolute_v3_are_intervals() {
        let run = |version| {
            stamps(&write_lines(version, &header_80x24(), |writer| {
                writer.output(Duration::from_millis(100), b"a").expect("a");
                writer.output(Duration::from_millis(500), b"b").expect("b");
                writer.output(Duration::from_millis(900), b"c").expect("c");
            }))
        };
        assert_eq!(run(CastVersion::V2), ["0.100", "0.500", "0.900"]);
        assert_eq!(run(CastVersion::V3), ["0.100", "0.400", "0.400"]);
    }

    #[test]
    fn utf8_split_across_two_chunks_emits_one_intact_char() {
        let bytes = "世".as_bytes();
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .output(Duration::from_millis(10), &bytes[..2])
                .expect("first half");
            writer
                .output(Duration::from_millis(20), &bytes[2..])
                .expect("second half");
            writer
                .output(Duration::from_millis(30), b"")
                .expect("empty");
        });
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains('世'), "{lines:?}");
        assert!(!lines[1].contains('\u{fffd}'), "{lines:?}");
    }

    #[test]
    fn invalid_or_truncated_utf8_becomes_replacement_char() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            // 0xff can never begin a sequence, so it must not be held back.
            writer
                .output(Duration::from_millis(5), b"a\xffb")
                .expect("chunk");
            // A dangling lead byte is flushed by `finish`.
            writer
                .output(Duration::from_millis(6), &"世".as_bytes()[..2])
                .expect("partial");
        });
        assert!(lines[1].contains("a\u{fffd}b"), "{lines:?}");
        assert!(lines[2].contains('\u{fffd}'), "{lines:?}");
    }

    #[test]
    fn millisecond_timebase_does_not_drift() {
        // 1005 us per event: not a whole millisecond, so an f64 accumulator
        // would visibly wander.
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            for k in 0..1000_u64 {
                writer
                    .output(Duration::from_micros(k * 1005), b"x")
                    .expect("event");
            }
        });
        assert_eq!(lines.len(), 1001, "header plus 1000 events");
        assert_eq!(stamp_of(lines.last().expect("last line")), "1.003");
    }

    #[test]
    fn times_are_monotonic_when_clock_goes_backwards() {
        let lines = write_lines(CastVersion::V3, &header_80x24(), |writer| {
            writer.output(Duration::from_millis(500), b"a").expect("a");
            writer.output(Duration::from_millis(100), b"b").expect("b");
        });
        assert_eq!(stamps(&lines), ["0.500", "0.000"]);
    }

    #[test]
    fn resize_and_exit_event_payloads() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .resize(Duration::from_millis(7), 120, 34)
                .expect("resize");
            writer.exit(Duration::from_millis(9), 130).expect("exit");
        });
        assert_eq!(lines[1], r#"[0.007, "r", "120x34"]"#);
        assert_eq!(lines[2], r#"[0.009, "x", "130"]"#);
    }

    #[test]
    fn read_cast_rejects_v1_and_skips_unknown_codes() {
        let v1 = r#"{"version":1,"width":80,"height":24,"stdout":[]}"#;
        let err = read_cast(v1.as_bytes()).expect_err("v1 is rejected");
        assert!(matches!(err, RecordError::Cast(_)), "{err:?}");

        let v2 = concat!(
            "{\"version\":2,\"width\":80,\"height\":24}\n",
            "[0.100, \"o\", \"a\"]\n",
            "[0.200, \"q\", \"who knows\"]\n",
            "[0.300, \"o\", \"b\"]\n",
        );
        let (header, events) = read_cast(v2.as_bytes()).expect("v2 parses");
        assert_eq!((header.cols, header.rows), (80, 24));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].time_ms, 100);
        assert_eq!(events[1].time_ms, 300);
        assert_eq!(events[1].data, "b");
    }

    #[test]
    fn read_cast_normalizes_v3_intervals_to_absolute_ms() {
        let v3 = concat!(
            "{\"version\":3,\"term\":{\"cols\":100,\"rows\":30},\"idle_time_limit\":2.0}\n",
            "# a comment line, legal anywhere but line 1\n",
            "[0.100, \"o\", \"a\"]\n",
            "[0.400, \"q\", \"unknown code still advances the clock\"]\n",
            "[1.500, \"x\", \"0\"]\n",
        );
        let (header, events) = read_cast(v3.as_bytes()).expect("v3 parses");
        assert_eq!((header.cols, header.rows), (100, 30));
        assert_eq!(header.idle_time_limit, Some(2.0));
        assert_eq!(
            events.iter().map(|e| e.time_ms).collect::<Vec<_>>(),
            [100, 2000]
        );
        assert_eq!(events[1].code, EventCode::Exit);
    }

    #[test]
    fn header_and_payload_round_trip_through_the_writer() {
        let theme = CastTheme {
            fg: [0xd0, 0xd0, 0xd0],
            bg: [0x10, 0x20, 0x30],
            palette: (0..16_u8).map(|i| [i, 0x40, 0x50]).collect(),
        };
        let header = CastHeader {
            theme: Some(theme),
            title: Some("a \"quoted\" title".to_owned()),
            timestamp: Some(1_700_000_000),
            ..header_80x24()
        };
        let payload = "\u{1b}[31mred\u{1b}[0m\r\n\u{7}";
        for version in [CastVersion::V2, CastVersion::V3] {
            let lines = write_lines(version, &header, |writer| {
                writer
                    .output(Duration::from_millis(1), payload.as_bytes())
                    .expect("payload");
            });
            assert!(
                lines[0].contains("\"palette\":\"#004050:#014050:"),
                "palette must be colon-delimited: {}",
                lines[0]
            );
            let (parsed, events) = read_cast(lines.join("\n").as_bytes()).expect("round trip");
            assert_eq!(parsed, header);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].data, payload);
        }
    }
}
