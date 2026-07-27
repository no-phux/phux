//! asciicast v2 / v3 reader and writer.
//!
//! asciicast is NDJSON: line 1 is a JSON object header, every later line is a
//! JSON array `[time, "code", "data"]`. The two versions differ in exactly
//! two ways that matter to us, and both are handled here rather than leaking
//! upward:
//!
//! * **Header shape.** v2 puts `width`/`height` flat on the header; v3 nests
//!   them as `term.cols`/`term.rows` and moves `theme` inside `term` too.
//! * **Timebase.** v2 event times are *absolute* seconds from session start;
//!   v3 event times are *relative* intervals since the previous event.
//!
//! v3 is therefore **not** backward compatible with v2 — a v2-only reader
//! that tolerates a v3 header reads intervals as absolute times and plays a
//! four-minute recording in a fraction of a second, which is worse than a
//! clean rejection. That is why [`CastVersion::V2`] is the default this
//! feature writes (ADR-0060): v2 is read by asciinema CLI 2.x *and* 3.x,
//! player >= 2.6, and server >= 20171105, and there is no consumer that reads
//! v3 but not v2.
//!
//! # Timebase, drift, and why it is integer milliseconds
//!
//! [`CastEvent::time_ms`] is absolute integer milliseconds from session
//! start, always, in both directions. Serialization divides by 1000 at the
//! very last moment using integer arithmetic (`{secs}.{millis:03}`), so
//! nothing ever accumulates a float. A writer that added `f64` seconds per
//! event would drift visibly over a long recording, and — worse — the `.cast`
//! and the GIF rendered from it would drift *differently* and stop agreeing
//! about when anything happened.
//!
//! Times are monotonic non-decreasing on write: a timestamp that goes
//! backwards (a clock adjustment mid-session) is clamped to the previous
//! value rather than emitting a negative v3 interval.
//!
//! # Input events are never captured
//!
//! Both spec versions instruct recorders not to record input by default, and
//! this feature has no opt-in flag. [`EventCode::Input`] exists so
//! [`read_cast`] can round-trip somebody else's recording; nothing in phux
//! ever writes one. Passwords do not go in recordings.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::time::Duration;

use crate::error::RecordError;
use crate::timeline::secs_to_ms;

/// Which asciicast revision to serialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CastVersion {
    /// asciicast v2: flat header dims, absolute event times. The default,
    /// and the only version every shipped asciinema consumer can read.
    #[default]
    V2,
    /// asciicast v3: `term`-nested header dims, relative event intervals.
    /// Needs asciinema CLI >= 3.0 / player >= 3.10.0 / server >= 20250509.
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

    /// Parse a wire character, or `None` for a code we do not know.
    ///
    /// Returning `None` rather than an error is deliberate: both spec
    /// versions mandate that readers tolerate unknown codes, because that is
    /// the format's only extension mechanism.
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
/// `palette` carries 8 or 16 entries (the ANSI names); anything else is
/// rejected on read, because the v2 serialization is a colon-delimited
/// string whose length is how a player tells the two cases apart.
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
/// Optional fields are written only when `Some` — never as `null`. asciinema
/// itself omits unset keys, and a `null` where a player expects a number is
/// the kind of thing that fails in one implementation and not another.
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
/// every event, so a session that crashes leaves a *playable prefix* rather
/// than a truncated JSON document. That durability is the whole point of the
/// line-delimited format, and it is why this type never buffers events.
///
/// # UTF-8 carry
///
/// PTY output is a raw byte stream, not UTF-8: a multi-byte character can and
/// does straddle two reads. [`CastWriter::output`] therefore appends to an
/// internal tail, emits the longest valid UTF-8 prefix, and *retains* an
/// incomplete trailing sequence for the next call. Calling
/// `String::from_utf8_lossy` per chunk instead would splice U+FFFD into every
/// box-drawing and emoji recording at the chunk boundaries.
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

    /// Milliseconds covered so far — the timestamp of the last emitted event.
    ///
    /// Exposed because the header's optional `duration` key cannot be written
    /// by a streaming writer (the header is already on disk by the time the
    /// duration is known). A caller that owns a seekable file and wants the
    /// key can read this at the end and rewrite line 1; omitting it is also
    /// correct, and players fall back to the last event's time.
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
    /// A tail still present here is genuinely truncated — the stream ended
    /// mid-character — so it is emitted as one U+FFFD rather than silently
    /// dropped: a recording that swallows its last byte is harder to debug
    /// than one that shows a replacement character.
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
        // Monotonic clamp: a backwards clock must not produce a negative v3
        // interval, and must not make a v2 player seek backwards.
        let ms = at_ms.max(self.last_ms);
        let stamp = match self.version {
            CastVersion::V2 => format_secs(ms),
            CastVersion::V3 => format_secs(ms - self.last_ms),
        };
        self.last_ms = ms;
        let line = format!("[{stamp}, \"{}\", {}]\n", code.as_str(), json_str(data));
        self.sink.write_all(line.as_bytes())?;
        // Flush per event: the streaming format's stated benefit is that a
        // crashed session leaves a playable prefix on disk.
        self.sink.flush()?;
        Ok(())
    }
}

/// Read a v2 or v3 asciicast, normalizing both onto absolute milliseconds.
///
/// Unknown event codes are skipped rather than rejected (their v3 interval
/// still advances the clock, so skipping one cannot shift the events after
/// it). Blank lines and `#` comment lines are ignored. asciicast v1 is
/// rejected outright: it is a single JSON document with a `stdout` array, not
/// NDJSON, and nothing has produced one since 2017.
pub fn read_cast<R: BufRead>(src: R) -> Result<(CastHeader, Vec<CastEvent>), RecordError> {
    let mut lines = src.lines();
    let mut header_line = None;
    for line in &mut lines {
        let line = line?;
        if !line.trim().is_empty() {
            header_line = Some(line);
            break;
        }
    }
    let Some(header_line) = header_line else {
        return Err(RecordError::Cast("input is empty".to_owned()));
    };

    let raw: serde_json::Value = serde_json::from_str(&header_line)
        .map_err(|err| RecordError::Cast(format!("header is not JSON: {err}")))?;
    let version = raw
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| RecordError::Cast("header has no numeric `version`".to_owned()))?;

    let header = match version {
        2 => parse_header_v2(&raw)?,
        3 => parse_header_v3(&raw)?,
        1 => {
            return Err(RecordError::Cast(
                "asciicast v1 is not supported; re-record or convert with `asciinema convert`"
                    .to_owned(),
            ));
        }
        other => {
            return Err(RecordError::Cast(format!(
                "unknown asciicast version {other}; this build reads v2 and v3"
            )));
        }
    };
    let relative = version == 3;

    let mut events = Vec::new();
    let mut clock = 0_u64;
    for line in lines {
        let line = line?;
        let trimmed = line.trim();
        // v3 permits `#` comments anywhere but line 1; v2 has no comment
        // syntax, so tolerating them there costs nothing and rejects nothing
        // real.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parsed: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|err| RecordError::Cast(format!("event line is not JSON: {err}")))?;
        let items = parsed
            .as_array()
            .ok_or_else(|| RecordError::Cast("event line is not a JSON array".to_owned()))?;
        let secs = items
            .first()
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| RecordError::Cast("event line has no numeric time".to_owned()))?;
        let code_text = items
            .get(1)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| RecordError::Cast("event line has no string code".to_owned()))?;
        let data = items
            .get(2)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();

        let ms = secs_to_ms(secs);
        clock = if relative {
            clock.saturating_add(ms)
        } else {
            // Absolute, but still clamped: a malformed file must not hand the
            // renderer a timeline that goes backwards.
            ms.max(clock)
        };
        // Advance the clock BEFORE the skip so an unknown code cannot shift
        // every later v3 event.
        let Some(code) = EventCode::from_code(code_text) else {
            continue;
        };
        events.push(CastEvent {
            time_ms: clock,
            code,
            data,
        });
    }

    Ok((header, events))
}

fn parse_header_v2(raw: &serde_json::Value) -> Result<CastHeader, RecordError> {
    let cols = dim(raw.get("width"), "width")?;
    let rows = dim(raw.get("height"), "height")?;
    Ok(CastHeader {
        cols,
        rows,
        timestamp: raw.get("timestamp").and_then(serde_json::Value::as_u64),
        idle_time_limit: raw
            .get("idle_time_limit")
            .and_then(serde_json::Value::as_f64),
        command: opt_string(raw.get("command")),
        title: opt_string(raw.get("title")),
        env: parse_env(raw.get("env")),
        theme: parse_theme(raw.get("theme")),
    })
}

fn parse_header_v3(raw: &serde_json::Value) -> Result<CastHeader, RecordError> {
    let term = raw
        .get("term")
        .ok_or_else(|| RecordError::Cast("v3 header has no `term` object".to_owned()))?;
    let cols = dim(term.get("cols"), "term.cols")?;
    let rows = dim(term.get("rows"), "term.rows")?;
    Ok(CastHeader {
        cols,
        rows,
        timestamp: raw.get("timestamp").and_then(serde_json::Value::as_u64),
        idle_time_limit: raw
            .get("idle_time_limit")
            .and_then(serde_json::Value::as_f64),
        command: opt_string(raw.get("command")),
        title: opt_string(raw.get("title")),
        env: parse_env(raw.get("env")),
        // v3 moved `theme` inside `term`.
        theme: parse_theme(term.get("theme")),
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
    // v2 serializes the palette as a colon-delimited string. Some producers
    // write an array instead; accept both on read, write only the string.
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

/// Render whole milliseconds as fixed-3-decimal seconds without touching a
/// float. `258425` becomes `258.425`, exactly, always.
fn format_secs(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Whole milliseconds of a `Duration`, saturating rather than wrapping.
fn duration_ms(at: Duration) -> u64 {
    u64::try_from(at.as_millis()).unwrap_or(u64::MAX)
}

/// JSON-escape a string, delegating every escaping rule to `serde_json`.
///
/// Recorded data is full of ESC (0x1b) and other C0 bytes; hand-rolled
/// escaping is exactly how a recording becomes unparseable.
fn json_str(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// Move every decodable character out of `tail` and into `out`, leaving only
/// an incomplete trailing sequence behind.
///
/// Genuinely invalid bytes (a bad lead byte, or four-plus bytes that can
/// never complete) become one U+FFFD each and are consumed; only a *prefix of
/// a valid sequence* is retained for the next chunk.
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
                    // Undecodable: report it and keep going, because the rest
                    // of the chunk is almost certainly fine.
                    out.push(char::REPLACEMENT_CHARACTER);
                    tail.drain(..valid.saturating_add(bad));
                } else {
                    // Merely incomplete: hold it for the next chunk.
                    tail.drain(..valid);
                    return;
                }
            }
        }
    }
}

/// Insertion-ordered JSON object builder.
///
/// `serde_json::Map` is a `BTreeMap` in this build (no `preserve_order`
/// feature), which would sort the header's keys alphabetically. Both spec
/// versions document a key order, and matching it makes a hand-diffed
/// recording readable, so the header is assembled by hand.
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
    // Colon-delimited, NOT an array: that is the v2 wire shape, and v3 kept
    // it when it moved `theme` under `term`.
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
            // `type` (the TERM name) has no home on `CastHeader`, so it is
            // never emitted; it is optional in the spec.
            if let Some(theme) = &header.theme {
                term.raw("theme", &serialize_theme(theme));
            }
            obj.raw("term", &term.finish());
            push_common_optionals(&mut obj, header);
        }
    }
    obj.finish()
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

    /// The first element of an event line, as the *literal text* that was
    /// written. Parsing it as a number first would erase exactly the thing
    /// these tests exist to check — `0.100` and `0.1` are the same f64.
    fn stamp_of(line: &str) -> String {
        let body = line.strip_prefix('[').expect("event line starts with [");
        let end = body.find(',').expect("event line has a comma");
        body.get(..end).expect("slice is in range").to_owned()
    }

    #[test]
    fn v2_header_has_flat_width_height() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |_| {});
        let head = lines.first().expect("header line");
        assert!(head.contains("\"version\":2"), "{head}");
        assert!(head.contains("\"width\":80"), "{head}");
        assert!(head.contains("\"height\":24"), "{head}");
        assert!(!head.contains("\"term\""), "{head}");
    }

    #[test]
    fn v3_header_nests_cols_rows_under_term() {
        let lines = write_lines(CastVersion::V3, &header_80x24(), |_| {});
        let head = lines.first().expect("header line");
        assert!(head.contains("\"version\":3"), "{head}");
        assert!(
            head.contains("\"term\":{\"cols\":80,\"rows\":24}"),
            "{head}"
        );
        assert!(!head.contains("\"width\""), "{head}");
    }

    #[test]
    fn v2_times_are_absolute_v3_are_intervals() {
        let stamps = |version| {
            let lines = write_lines(version, &header_80x24(), |writer| {
                writer.output(Duration::from_millis(100), b"a").expect("a");
                writer.output(Duration::from_millis(500), b"b").expect("b");
                writer.output(Duration::from_millis(900), b"c").expect("c");
            });
            lines
                .iter()
                .skip(1)
                .map(|line| stamp_of(line))
                .collect::<Vec<_>>()
        };
        assert_eq!(stamps(CastVersion::V2), ["0.100", "0.500", "0.900"]);
        assert_eq!(stamps(CastVersion::V3), ["0.100", "0.400", "0.400"]);
    }

    #[test]
    fn optional_header_keys_are_omitted_not_null() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |_| {});
        let head = lines.first().expect("header line");
        assert!(!head.contains("null"), "{head}");
        for key in [
            "timestamp",
            "idle_time_limit",
            "command",
            "title",
            "env",
            "theme",
        ] {
            assert!(!head.contains(key), "unset `{key}` leaked into {head}");
        }
    }

    #[test]
    fn v2_theme_palette_is_colon_delimited() {
        let header = CastHeader {
            theme: Some(CastTheme {
                fg: [0xd0, 0xd0, 0xd0],
                bg: [0, 0, 0],
                palette: (0..8_u8).map(|i| [i, i, i]).collect(),
            }),
            ..header_80x24()
        };
        let lines = write_lines(CastVersion::V2, &header, |_| {});
        let head = lines.first().expect("header line");
        assert!(
            head.contains(
                "\"palette\":\"#000000:#010101:#020202:#030303:#040404:#050505:#060606:#070707\""
            ),
            "{head}"
        );
        assert!(head.contains("\"fg\":\"#d0d0d0\""), "{head}");
    }

    #[test]
    fn utf8_split_across_two_chunks_emits_one_intact_char() {
        // U+4E16 is three bytes; split it 2 + 1 the way a PTY read would.
        let bytes = "世".as_bytes();
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .output(Duration::from_millis(10), &bytes[..2])
                .expect("first half");
            writer
                .output(Duration::from_millis(20), &bytes[2..])
                .expect("second half");
        });
        let events: Vec<&String> = lines.iter().skip(1).collect();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].contains('世'), "{events:?}");
        assert!(!events[0].contains('\u{fffd}'), "{events:?}");
    }

    #[test]
    fn invalid_utf8_becomes_replacement_char_not_dropped() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            // 0xff can never begin a UTF-8 sequence, so it is invalid rather
            // than incomplete and must not be held back forever.
            writer
                .output(Duration::from_millis(5), b"a\xffb")
                .expect("chunk");
        });
        let event = lines.get(1).expect("one event");
        assert!(event.contains('\u{fffd}'), "{event}");
        assert!(event.contains('a') && event.contains('b'), "{event}");
    }

    #[test]
    fn finish_flushes_residual_utf8_tail() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .output(Duration::from_millis(5), &"世".as_bytes()[..2])
                .expect("partial");
        });
        let event = lines.get(1).expect("finish emits the residual tail");
        assert!(event.contains('\u{fffd}'), "{event}");
    }

    #[test]
    fn millisecond_timebase_does_not_drift() {
        // 1005 us per event: a period that is deliberately not a whole
        // millisecond, so an f64 accumulator would visibly wander.
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            for k in 0..1000_u64 {
                writer
                    .output(Duration::from_micros(k * 1005), b"x")
                    .expect("event");
            }
        });
        assert_eq!(lines.len(), 1001, "header plus 1000 events");
        let last = lines.last().expect("last line");
        assert_eq!(stamp_of(last), "1.003");
    }

    #[test]
    fn times_are_monotonic_when_clock_goes_backwards() {
        let lines = write_lines(CastVersion::V3, &header_80x24(), |writer| {
            writer.output(Duration::from_millis(500), b"a").expect("a");
            writer.output(Duration::from_millis(100), b"b").expect("b");
        });
        let stamps: Vec<String> = lines.iter().skip(1).map(|line| stamp_of(line)).collect();
        // The backwards event is clamped to the previous time, which in v3
        // means a zero interval rather than a negative one.
        assert_eq!(stamps, ["0.500", "0.000"]);
    }

    #[test]
    fn writer_emits_no_hash_comment_lines() {
        let lines = write_lines(CastVersion::V3, &header_80x24(), |writer| {
            writer.output(Duration::from_millis(1), b"hi").expect("o");
            writer
                .marker(Duration::from_millis(2), "chapter")
                .expect("m");
            writer.exit(Duration::from_millis(3), 0).expect("x");
        });
        assert!(lines.iter().all(|line| !line.starts_with('#')), "{lines:?}");
    }

    #[test]
    fn resize_event_data_is_colsxrows() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .resize(Duration::from_millis(7), 120, 34)
                .expect("resize");
        });
        let event = lines.get(1).expect("one event");
        assert!(event.contains("\"r\", \"120x34\""), "{event}");
    }

    #[test]
    fn exit_event_carries_the_stringified_status() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer.exit(Duration::from_millis(9), 130).expect("exit");
        });
        let event = lines.get(1).expect("one event");
        assert!(event.contains("\"x\", \"130\""), "{event}");
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
            "[0.400, \"o\", \"b\"]\n",
            "[1.500, \"x\", \"0\"]\n",
        );
        let (header, events) = read_cast(v3.as_bytes()).expect("v3 parses");
        assert_eq!((header.cols, header.rows), (100, 30));
        assert_eq!(header.idle_time_limit, Some(2.0));
        assert_eq!(
            events.iter().map(|e| e.time_ms).collect::<Vec<_>>(),
            [100, 500, 2000]
        );
        assert_eq!(events[2].code, EventCode::Exit);
    }

    #[test]
    fn read_cast_round_trips_a_theme_through_the_v2_writer() {
        let theme = CastTheme {
            fg: [0xd0, 0xd0, 0xd0],
            bg: [0x10, 0x20, 0x30],
            palette: (0..16_u8).map(|i| [i, 0x40, 0x50]).collect(),
        };
        let header = CastHeader {
            theme: Some(theme.clone()),
            title: Some("a \"quoted\" title".to_owned()),
            timestamp: Some(1_700_000_000),
            ..header_80x24()
        };
        let lines = write_lines(CastVersion::V2, &header, |_| {});
        let text = lines.join("\n");
        let (parsed, _) = read_cast(text.as_bytes()).expect("round trip");
        assert_eq!(parsed.theme, Some(theme));
        assert_eq!(parsed.title.as_deref(), Some("a \"quoted\" title"));
        assert_eq!(parsed.timestamp, Some(1_700_000_000));
    }

    #[test]
    fn escaped_control_bytes_survive_a_round_trip() {
        let payload = "\u{1b}[31mred\u{1b}[0m\r\n\u{7}";
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer
                .output(Duration::from_millis(1), payload.as_bytes())
                .expect("payload");
        });
        let (_, events) = read_cast(lines.join("\n").as_bytes()).expect("round trip");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, payload);
    }

    #[test]
    fn empty_output_chunk_emits_no_event() {
        let lines = write_lines(CastVersion::V2, &header_80x24(), |writer| {
            writer.output(Duration::from_millis(1), b"").expect("empty");
        });
        assert_eq!(lines.len(), 1, "header only: {lines:?}");
    }
}
