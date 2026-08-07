//! `phux rec` — capture a pane and export it as a cast, a GIF, or an APNG.
//!
//! This module is the CLI half of built-in session recording (ADR-0060).
//! It owns two things:
//!
//! * [`run_rec`], the headless verb: resolve a selector, subscribe to the
//!   pane as a pure observer via `phux_client::record`, write the asciicast,
//!   and hand it to `phux_record::render` for the animation.
//! * [`finalize`], the tail of the interactive `--rec` path: the attach
//!   driver's tee has already streamed the cast to disk by the time the TUI
//!   comes down, so all that is left is the export and the one-liner.
//!
//! Both go through [`spec::plan`], so `demo.png` means the same thing on
//! either surface.
//!
//! # Output discipline
//!
//! stdout carries exactly one thing: the completion one-liner, or — under
//! `--json` — one JSON object and nothing else. Progress and diagnostics go
//! to stderr, and *only* on the headless path: the `--rec` attach path owns
//! the alt screen, so a stray stderr byte while the session is up would
//! corrupt the display. Everything the interactive path has to say it says
//! after the raw-mode guard has dropped.

pub(crate) mod spec;

use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};
use phux_record::error::RecordError;
use phux_record::render::{OutputFormat, RenderOptions, render_cast};
use phux_record::timeline::clamp_idle;
use phux_server::runtime::default_socket_path;

use crate::commands::{RecFormat, cli_runtime, json_err, parse_selector, resolve_target};

pub(crate) use spec::RecordSpec;

/// How often the headless capture repaints its stderr progress line.
const PROGRESS_PERIOD: Duration = Duration::from_millis(250);

/// Everything `phux rec` was asked to do.
///
/// A struct rather than eleven positional parameters: `clippy::too_many_
/// arguments` is pedantic and warnings are denied, and a call site with
/// eleven bare values is unreadable regardless of what the linter thinks.
#[derive(Debug)]
pub(crate) struct RecArgs<'a> {
    /// Pane selector; `None` means the focused pane.
    pub(crate) target: Option<&'a str>,
    /// Where the artifact goes. Its extension picks the format unless
    /// `format` overrides it.
    pub(crate) out: &'a Path,
    /// Explicit output format.
    pub(crate) format: Option<RecFormat>,
    /// Re-render this existing cast instead of capturing anything.
    pub(crate) from: Option<&'a Path>,
    /// Stop the capture after this many seconds.
    pub(crate) duration: Option<u64>,
    /// Animation sample rate.
    pub(crate) fps: u8,
    /// Collapse pauses longer than this many seconds; `0` disables.
    pub(crate) idle_limit: f64,
    /// Stop encoding and warn once the output reaches this many bytes.
    pub(crate) max_bytes: u64,
    /// asciicast format version to write.
    pub(crate) cast_version: u8,
    /// Emit one JSON object on stdout instead of the human one-liner.
    pub(crate) json: bool,
    /// Override the UDS path.
    pub(crate) socket: Option<PathBuf>,
}

/// Run `phux rec`.
///
/// Capture is a *pure observer*: the pane is subscribed to with
/// `ATTACH_TERMINAL`, never attached to and never resized, so it is safe
/// against a live session a human is using. See `phux_client::record` for the
/// prohibition that makes that true and the regression guard that keeps it.
///
/// Ctrl-C during a capture is a **success**: the user asked the recording to
/// stop, and what was captured up to that point is written and exported. That
/// matches `phux watch`, and it is the only behaviour that makes an
/// open-ended `phux rec -o demo.gif` usable.
pub(crate) fn run_rec(args: RecArgs<'_>) -> ExitCode {
    let spec = match spec::plan(args.out, args.format) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    let version = cast_version(args.cast_version);
    let idle_limit = idle_limit(args.idle_limit);

    // Source the timeline: either an existing cast (`--from`, no server in
    // the picture at all) or a live capture.
    let (header, mut events) = match args.from {
        Some(from) => match read_cast_file(from) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("phux: rec: cannot read {}: {err}", from.display());
                return ExitCode::FAILURE;
            }
        },
        // `args.socket` moves in here, which is the whole reason this takes
        // the args by value: the capture owns the path it dials.
        None => match capture(
            args.target,
            args.socket,
            args.duration.map(Duration::from_secs),
            args.json,
            idle_limit,
        ) {
            Ok(pair) => pair,
            Err(code) => return code,
        },
    };

    // Clamped once, on the shared list, before both the cast write and the
    // render — so the archival `.cast` and the GIF derived from it can never
    // disagree about how long a pause was.
    clamp_idle(&mut events, idle_limit);

    if let Err(err) = write_cast_file(&spec.cast_path, &header, &events, version) {
        eprintln!(
            "phux: rec failed: writing {}: {err}",
            spec.cast_path.display()
        );
        return ExitCode::FAILURE;
    }

    let opts = RenderOptions {
        fps: args.fps,
        idle_limit,
        max_bytes: args.max_bytes,
        ..RenderOptions::default()
    };
    match export(&spec, &header, &events, &opts) {
        Ok(outcome) => {
            report(&outcome, args.json);
            ExitCode::SUCCESS
        }
        Err(err) => {
            report_export_failure(&spec, &err);
            ExitCode::FAILURE
        }
    }
}

/// Export the cast the interactive `--rec` tee already wrote, and report it.
///
/// Called by the attach path once the TUI is down — raw mode restored, alt
/// screen dropped — because it prints to stdout. The interactive surface has
/// no `--fps` / `--idle-limit` / `--max-bytes` knobs, so the render uses
/// [`RenderOptions::default`]; a user who wants to re-render at a different
/// rate keeps the `.cast` and runs `phux rec --from`.
pub(crate) fn finalize(spec: &RecordSpec) {
    let (header, events) = match read_cast_file(&spec.cast_path) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!(
                "phux: recording at {} could not be read back: {err}",
                spec.cast_path.display()
            );
            return;
        }
    };
    if events.is_empty() {
        // A session that produced no bytes at all: an attach that never got
        // past the handshake, or a client that exited before the first frame.
        // "wrote demo.gif (0 frames)" would read as success, and a one-frame
        // animation of a blank grid is not worth writing.
        eprintln!("phux: --rec captured no output; nothing was exported");
        if spec.cast_is_temp {
            let _ = std::fs::remove_file(&spec.cast_path);
        }
        return;
    }
    match export(spec, &header, &events, &RenderOptions::default()) {
        Ok(outcome) => report(&outcome, false),
        Err(err) => report_export_failure(spec, &err),
    }
}

/// Capture a live pane into an in-memory timeline.
///
/// Errors are reported here rather than propagated because each one has its
/// own actionable phrasing: a missing server names the socket, an unresolvable
/// selector names the target.
fn capture(
    target: Option<&str>,
    socket: Option<PathBuf>,
    max_duration: Option<Duration>,
    json: bool,
    idle_limit: Option<f64>,
) -> Result<(CastHeader, Vec<CastEvent>), ExitCode> {
    let selector = parse_selector(target)?;
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = cli_runtime()?;

    rt.block_on(async move {
        let terminal_id = resolve_target(&socket_path, &selector, "rec", json).await?;

        // Progress is a live counter on stderr, rewritten in place. It is
        // suppressed under `--json` so a consumer that reads stderr for
        // diagnostics does not have to filter a spinner out of it.
        let mut last_tick = None::<Duration>;
        let progress = |elapsed: Duration, events: usize| {
            if json {
                return;
            }
            if last_tick.is_some_and(|last| elapsed.saturating_sub(last) < PROGRESS_PERIOD) {
                return;
            }
            last_tick = Some(elapsed);
            eprint!("\rrecording... {}s ({events} events)", elapsed.as_secs());
            let _ = std::io::stderr().flush();
        };

        let recorded =
            phux_client::record::record_terminal(&socket_path, terminal_id, max_duration, progress)
                .await;
        if !json {
            // Close the progress line so the completion one-liner does not
            // land on top of it.
            eprintln!();
        }

        match recorded {
            Ok(recording) => into_timeline(recording, idle_limit).map_err(|message| {
                eprintln!("phux: rec failed: {message}");
                ExitCode::FAILURE
            }),
            Err(err @ (AttachError::Io(_) | AttachError::Disconnected)) => {
                Err(json_err::report_no_server(json, &err, &socket_path, "rec"))
            }
            Err(err) => {
                eprintln!("phux: rec failed: {err}");
                Err(ExitCode::FAILURE)
            }
        }
    })
}

/// Project a completed capture onto a cast header and timeline.
///
/// Zero dimensions are refused rather than written. `HeadlessRecording` seeds
/// `cols`/`rows` from the pane's priming `TERMINAL_SNAPSHOT`, so a zero means
/// that snapshot never landed — which also means the recording is missing the
/// screen state it opens on. Writing it anyway produces a `.cast` no player
/// can lay out and a 0x0 animation that exits 0 and looks like success, which
/// is strictly worse than a named failure.
fn into_timeline(
    recording: phux_client::record::HeadlessRecording,
    idle_limit: Option<f64>,
) -> Result<(CastHeader, Vec<CastEvent>), String> {
    if recording.cols == 0 || recording.rows == 0 {
        return Err(format!(
            "the pane's opening snapshot never arrived (server reported a \
             {}x{} grid), so there is nothing playable to write",
            recording.cols, recording.rows
        ));
    }
    Ok((
        CastHeader {
            cols: recording.cols,
            rows: recording.rows,
            timestamp: Some(recording.started_unix),
            idle_time_limit: idle_limit,
            command: None,
            title: None,
            // The recorder is an observer on another process's terminal; its
            // own environment says nothing true about the pane, so claiming a
            // TERM here would be a lie.
            env: std::collections::BTreeMap::new(),
            theme: None,
        },
        recording.events,
    ))
}

/// Read an asciicast off disk, normalized onto absolute milliseconds.
fn read_cast_file(path: &Path) -> Result<(CastHeader, Vec<CastEvent>), RecordError> {
    let file = std::fs::File::open(path)?;
    read_cast(BufReader::new(file))
}

/// Serialize a header and timeline to `path` as an asciicast.
fn write_cast_file(
    path: &Path,
    header: &CastHeader,
    events: &[CastEvent],
    version: CastVersion,
) -> Result<(), RecordError> {
    let sink = BufWriter::new(std::fs::File::create(path)?);
    let mut writer = CastWriter::new(sink, header, version)?;
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
            // A non-numeric exit status is not worth failing a whole
            // recording over, and 0 is the only defensible substitute.
            EventCode::Exit => writer.exit(at, event.data.trim().parse::<i32>().unwrap_or(0))?,
            // Input is never written. Both asciicast versions tell recorders
            // not to capture it by default, and phux has no opt-in: passwords
            // do not go in recordings.
            EventCode::Input => {}
        }
    }
    let mut sink = writer.finish()?;
    // Explicit, because a `BufWriter` that flushes in `Drop` swallows the
    // ENOSPC that would otherwise be the one error the user needs to see.
    sink.flush()?;
    Ok(())
}

/// Split an asciicast `"COLSxROWS"` resize payload.
fn parse_resize(data: &str) -> Option<(u16, u16)> {
    let (cols, rows) = data.trim().split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// What an export produced, in the shape both report forms need.
#[derive(Debug, Clone)]
struct RecOutcome {
    path: PathBuf,
    format: OutputFormat,
    bytes: u64,
    duration_ms: u64,
    frames: u32,
    cols: u16,
    rows: u16,
    truncated: bool,
}

/// Produce `spec.final_path` from the timeline, and clean up the intermediate.
///
/// For [`OutputFormat::Cast`] the cast written upstream *is* the artifact, so
/// this only measures it. Otherwise the animation is rendered into memory and
/// written in one shot: `render_cast` consumes its sink by value and never
/// hands it back, so a `BufWriter` around the file would flush in `Drop` and
/// lose the error. The buffer is bounded by `max_bytes` — it is the artifact
/// the user is about to write, not a film of decoded frames.
fn export(
    spec: &RecordSpec,
    header: &CastHeader,
    events: &[CastEvent],
    opts: &RenderOptions,
) -> Result<RecOutcome, RecordError> {
    let duration_ms = events.last().map_or(0, |event| event.time_ms);

    if spec.format == OutputFormat::Cast {
        let bytes = std::fs::metadata(&spec.cast_path)?.len();
        return Ok(RecOutcome {
            path: spec.final_path.clone(),
            format: spec.format,
            bytes,
            duration_ms,
            // A cast has no frames; the honest count of what it contains is
            // its events, and reporting 0 would read as "nothing captured".
            frames: u32::try_from(events.len()).unwrap_or(u32::MAX),
            cols: header.cols,
            rows: header.rows,
            truncated: false,
        });
    }

    let mut buffer = Vec::new();
    let stats = render_cast(header, events, &mut buffer, spec.format, opts)?;
    std::fs::write(&spec.final_path, &buffer)?;

    if spec.cast_is_temp {
        // Only after the artifact is on disk. A failed render keeps the
        // intermediate (see `report_export_failure`) — losing a live capture
        // to an encoder bug is unacceptable in a way a stale temp file is not.
        let _ = std::fs::remove_file(&spec.cast_path);
    }

    Ok(RecOutcome {
        path: spec.final_path.clone(),
        format: spec.format,
        bytes: stats.bytes,
        duration_ms: stats.duration_ms,
        frames: stats.frames,
        cols: header.cols,
        rows: header.rows,
        truncated: stats.truncated,
    })
}

/// Report a failed export, naming the retained capture.
fn report_export_failure(spec: &RecordSpec, err: &RecordError) {
    eprintln!("phux: rec failed: {err}");
    if spec.cast_is_temp && spec.cast_path.exists() {
        eprintln!(
            "      the capture is intact at {}; re-render it with \
             `phux rec --from {} -o {}`",
            spec.cast_path.display(),
            spec.cast_path.display(),
            spec.final_path.display()
        );
    }
}

/// Print the completion one-liner, or the `--json` object.
fn report(outcome: &RecOutcome, json: bool) {
    if json {
        outln!("{}", rec_json(outcome));
        return;
    }

    outln!(
        "phux: wrote {} ({}, {} frames, {})",
        outcome.path.display(),
        human_bytes(outcome.bytes),
        outcome.frames,
        human_duration(outcome.duration_ms),
    );
    if outcome.truncated {
        eprintln!(
            "phux: rec: stopped at the --max-bytes limit; the artifact is complete but short"
        );
    }
}

/// The `phux rec --json` result document. Pure, so the shape (including
/// `schema_version`) is unit-testable without a capture.
fn rec_json(outcome: &RecOutcome) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "path": outcome.path.to_string_lossy(),
        "format": outcome.format.as_str(),
        "bytes": outcome.bytes,
        "duration_ms": outcome.duration_ms,
        "frames": outcome.frames,
        "cols": outcome.cols,
        "rows": outcome.rows,
        "truncated": outcome.truncated,
    })
}

/// Binary-prefix byte count, one decimal place above a kibibyte.
fn human_bytes(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "display only; a byte count large enough to lose f64 precision is not a recording"
    )]
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", value / 1024.0)
    } else {
        format!("{:.1} MiB", value / (1024.0 * 1024.0))
    }
}

/// Wall duration as seconds with one decimal, the scale a recording is read at.
fn human_duration(ms: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "display only; see human_bytes")]
    let secs = ms as f64 / 1000.0;
    format!("{secs:.1}s")
}

/// Map the `--cast-version` integer clap already range-checked.
const fn cast_version(version: u8) -> CastVersion {
    // clap constrains this to 2..=3, so anything else cannot reach here;
    // v2 is the safe default either way (ADR-0060: v3 is not backward
    // compatible and every consumer that reads v3 also reads v2).
    if version == 3 {
        CastVersion::V3
    } else {
        CastVersion::V2
    }
}

/// Map the `--idle-limit` seconds value onto the clamp's `Option`.
///
/// `0` (and anything non-positive or non-finite) disables the clamp, which is
/// how a user asks for the timeline exactly as it happened.
fn idle_limit(secs: f64) -> Option<f64> {
    (secs.is_finite() && secs > 0.0).then_some(secs)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::{
        RecOutcome, cast_version, human_bytes, human_duration, idle_limit, into_timeline,
        parse_resize, rec_json,
    };
    use phux_client::record::HeadlessRecording;
    use phux_record::cast::{CastEvent, CastVersion, EventCode};
    use phux_record::render::OutputFormat;

    /// `phux rec --json` pins `schema_version` 1 plus the documented result
    /// fields.
    #[test]
    fn rec_json_pins_the_contract_shape() {
        let outcome = RecOutcome {
            path: "demo.gif".into(),
            format: OutputFormat::Gif,
            bytes: 188_742,
            duration_ms: 42_130,
            frames: 211,
            cols: 120,
            rows: 34,
            truncated: false,
        };
        let doc = rec_json(&outcome);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["path"], "demo.gif");
        assert_eq!(doc["format"], "gif");
        assert_eq!(doc["bytes"], 188_742);
        assert_eq!(doc["duration_ms"], 42_130);
        assert_eq!(doc["frames"], 211);
        assert_eq!(doc["cols"], 120);
        assert_eq!(doc["rows"], 34);
        assert_eq!(doc["truncated"], false);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(9));
    }

    /// A capture with `cols`x`rows` and one output event.
    fn recording(cols: u16, rows: u16) -> HeadlessRecording {
        HeadlessRecording {
            cols,
            rows,
            started_unix: 1_700_000_000,
            events: vec![CastEvent {
                time_ms: 0,
                code: EventCode::Output,
                data: "hi".to_owned(),
            }],
        }
    }

    #[test]
    fn timeline_carries_the_capture_dims_and_start_time() {
        let (header, events) =
            into_timeline(recording(120, 34), Some(2.0)).expect("usable capture");
        assert_eq!((header.cols, header.rows), (120, 34));
        assert_eq!(header.timestamp, Some(1_700_000_000));
        assert_eq!(header.idle_time_limit, Some(2.0));
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn zero_dimensions_are_refused_rather_than_written() {
        // A zero grid means the priming TERMINAL_SNAPSHOT never landed, so
        // the capture is missing the screen it opens on. A 0x0 artifact that
        // exits 0 is worse than a named failure.
        for dims in [(0, 24), (80, 0), (0, 0)] {
            let refused = into_timeline(recording(dims.0, dims.1), None);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|message| message.contains("opening snapshot")),
                "{dims:?} should be refused with a diagnosable message, got {refused:?}"
            );
        }
    }

    #[test]
    fn idle_limit_zero_disables_the_clamp() {
        assert_eq!(idle_limit(2.0), Some(2.0));
        assert_eq!(idle_limit(0.0), None);
        assert_eq!(idle_limit(-1.0), None);
        assert_eq!(idle_limit(f64::NAN), None);
    }

    #[test]
    fn cast_version_defaults_to_v2() {
        assert_eq!(cast_version(2), CastVersion::V2);
        assert_eq!(cast_version(3), CastVersion::V3);
        // Out of clap's range, so unreachable in practice — but the fallback
        // must be the interoperable version, never the newer one.
        assert_eq!(cast_version(9), CastVersion::V2);
    }

    #[test]
    fn resize_payload_parses_colsxrows() {
        assert_eq!(parse_resize("100x30"), Some((100, 30)));
        assert_eq!(parse_resize(" 80x24 "), Some((80, 24)));
        assert_eq!(parse_resize("80"), None);
        assert_eq!(parse_resize("axb"), None);
    }

    #[test]
    fn human_bytes_switches_unit_at_the_binary_prefixes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(184_320), "180.0 KiB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn human_duration_reads_in_seconds() {
        assert_eq!(human_duration(42_130), "42.1s");
        assert_eq!(human_duration(0), "0.0s");
    }
}
