//! Cast readback/re-export and rendered artifacts, including interactive finalization.

use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};
use phux_record::error::RecordError;
use phux_record::render::{OutputFormat, RenderOptions, render_cast};

use super::spec::RecordSpec;

/// Export the cast the interactive `--rec` tee wrote, once the TUI is down.
/// Uses [`RenderOptions::default`]; re-render with `phux rec --from`.
pub(crate) fn finalize(spec: &RecordSpec) {
    let (header, events) = match read_cast_file(&spec.cast_path) {
        Ok(pair) => pair,
        Err(err) => {
            error!(
                "recording at {} could not be read back: {err}",
                spec.cast_path.display()
            );
            return;
        }
    };
    if events.is_empty() {
        // No bytes at all (an attach that never got past the handshake): report,
        // don't write an empty animation.
        warn!("--rec captured no output; nothing was exported");
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
/// Read an asciicast off disk, normalized onto absolute milliseconds.
pub(super) fn read_cast_file(path: &Path) -> Result<(CastHeader, Vec<CastEvent>), RecordError> {
    let file = std::fs::File::open(path)?;
    read_cast(BufReader::new(file))
}

/// Serialize a header and timeline to `path` as an asciicast.
pub(super) fn write_cast_file(
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
            EventCode::Exit => {
                let status = event.data.trim().parse::<i32>().map_err(|_| {
                    RecordError::Cast(format!("unsupported exit event data {:?}", event.data))
                })?;
                writer.exit(at, status)?;
            }
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
pub(super) fn parse_resize(data: &str) -> Option<(u16, u16)> {
    let (cols, rows) = data.trim().split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// What an export produced, in the shape both report forms need.
#[derive(Debug, Clone)]
pub(super) struct RecOutcome {
    pub(super) path: PathBuf,
    pub(super) format: OutputFormat,
    pub(super) bytes: u64,
    pub(super) duration_ms: u64,
    pub(super) frames: u32,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) truncated: bool,
}

/// Produce `spec.final_path` and clean up the intermediate. A cast is
/// already the artifact; an animation is rendered into a buffer bounded by
/// `max_bytes` and written once (a `BufWriter` sink would lose the error in
/// `Drop`).
pub(super) fn export(
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
pub(super) fn report_export_failure(spec: &RecordSpec, err: &RecordError) {
    error!("rec failed: {err}");
    if spec.cast_is_temp && spec.cast_path.exists() {
        hint!(
            "the capture is intact at {}; re-render it with \
             `phux rec --from {} -o {}`",
            spec.cast_path.display(),
            spec.cast_path.display(),
            spec.final_path.display()
        );
    }
}

/// Print the completion one-liner, or the `--json` object.
pub(super) fn report(outcome: &RecOutcome, json: bool) {
    if json {
        outln!("{}", rec_json(outcome));
        return;
    }

    let frames = if outcome.frames == 1 {
        "frame"
    } else {
        "frames"
    };
    outln!(
        "phux: wrote {} ({}, {} {frames}, {})",
        outcome.path.display(),
        human_bytes(outcome.bytes),
        outcome.frames,
        human_duration(outcome.duration_ms),
    );
    if outcome.truncated {
        warn!("rec: stopped at the --max-bytes limit; the artifact is complete but short");
    }
}

/// The `phux rec --json` result document. Pure, so the shape (including
/// `schema_version`) is unit-testable without a capture.
pub(super) fn rec_json(outcome: &RecOutcome) -> serde_json::Value {
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
pub(super) fn human_bytes(bytes: u64) -> String {
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
