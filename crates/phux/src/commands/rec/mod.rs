//! `phux rec` — capture a pane and export it as a cast, a GIF, or an APNG
//! (ADR-0060). [`run_rec`] is the headless verb (a pure observer via
//! `phux_client::record`, or a zero-vote composited attach); [`finalize`] is
//! the tail of the interactive `--rec` path, whose cast is already on disk.
//! Both go through [`spec::plan`]. [`run_mark`] posts an asciicast `m` event
//! to every recording in progress.
//!
//! stdout carries only the completion line (or one JSON object under
//! `--json`); diagnostics go to stderr, and only on the headless path, since
//! `--rec` attach owns the alt screen until it exits.

pub(crate) mod spec;

use std::io::{self, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};
use phux_record::error::RecordError;
use phux_record::render::{OutputFormat, RenderOptions, render_cast};
use phux_record::timeline::clamp_idle;
use phux_server::runtime::default_socket_path;
use usage::Subcommands;

use crate::commands::{RecFormat, cli_runtime, json_err, parse_selector, resolve_target};

pub(crate) use spec::RecordSpec;

/// How often the headless capture repaints its stderr progress line.
const PROGRESS_PERIOD: Duration = Duration::from_millis(250);

/// Pixel size of a composited capture when `--size` is omitted: 1080p.
const DEFAULT_COMPOSITE_WIDTH_PX: u32 = 1920;
const DEFAULT_COMPOSITE_HEIGHT_PX: u32 = 1080;

/// Longest marker label `phux rec mark` accepts.
const MARK_LABEL_MAX_CHARS: usize = 256;

/// Subcommands of `phux rec`. Absent, the verb captures or re-renders.
#[derive(Debug, Subcommands)]
pub(crate) enum RecAction {
    /// Post an asciicast marker to every recording in progress.
    Mark {
        /// Label stored on the marker. At most 256 characters; empty is allowed.
        label: String,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

/// Everything `phux rec` was asked to do.
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
    /// Capture the session composite at a virtual viewport.
    pub(crate) composited: bool,
    /// `WIDTHxHEIGHT` pixels for [`Self::composited`]. `None` is `1920x1080`.
    pub(crate) size: Option<&'a str>,
    /// Render-time factor for marker spans. The archival cast is unchanged.
    pub(crate) speed_marked: Option<f64>,
    /// Draw opening-marker labels while rendering GIF/APNG.
    pub(crate) marker_labels: bool,
}

/// Run `phux rec`. Capture subscribes with `ATTACH_RESOURCE` and never
/// attaches or resizes, so it is safe on a live session. Ctrl-C is success: what
/// was captured is written and exported.
pub(crate) fn run_rec(args: RecArgs<'_>) -> ExitCode {
    let speed_marked = match checked_speed(args.speed_marked, args.json) {
        Ok(speed) => speed,
        Err(code) => return code,
    };
    let spec = match spec::plan(args.out, args.format) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    let version = cast_version(args.cast_version);
    let idle_limit = idle_limit(args.idle_limit);

    // Source the timeline: an existing cast (`--from`, no server), a
    // composited session (`--composited`, zero viewport), or one pane.
    let (mut header, mut events) = if let Some(from) = args.from {
        match read_cast_file(from) {
            Ok(pair) => pair,
            Err(err) => {
                error!("rec: cannot read {}: {err}", from.display());
                return ExitCode::FAILURE;
            }
        }
    } else if args.composited {
        match capture_composited(
            args.target,
            args.socket,
            &spec.cast_path,
            args.size,
            version,
            args.duration.map(Duration::from_secs),
            args.json,
        ) {
            Ok(pair) => pair,
            Err(code) => return code,
        }
    } else {
        // `args.socket` moves in here, which is the whole reason this takes
        // the args by value: the capture owns the path it dials.
        match capture(
            args.target,
            args.socket,
            args.duration.map(Duration::from_secs),
            args.json,
            idle_limit,
        ) {
            Ok(pair) => pair,
            Err(code) => return code,
        }
    };

    // A fresh capture records the clamp it applied. `--from` keeps the
    // limit the source cast already declares.
    if args.from.is_none() {
        header.idle_time_limit = idle_limit;
    }

    // Clamped once, on the shared list, before both the cast write and the
    // render — so the archival `.cast` and the GIF derived from it can never
    // disagree about how long a pause was. Marker speed is render-only and
    // is applied inside `render_cast`, so this file stays re-renderable.
    clamp_idle(&mut events, idle_limit);

    if let Err(err) = write_cast_file(&spec.cast_path, &header, &events, version) {
        error!("rec failed: writing {}: {err}", spec.cast_path.display());
        return ExitCode::FAILURE;
    }

    if !args.json
        && spec.format == OutputFormat::Cast
        && (speed_marked.is_some() || args.marker_labels)
    {
        info!(
            "rec: --speed-marked and --marker-labels apply when rendering a GIF or APNG; \
             {path} keeps the original timing and marker events. Re-render with \
             `phux rec --from {path} -o demo.gif --speed-marked 2`",
            path = spec.cast_path.display(),
        );
    }

    let opts = RenderOptions {
        fps: args.fps,
        idle_limit,
        max_bytes: args.max_bytes,
        speed_marked,
        marker_labels: args.marker_labels,
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

/// Capture a live pane into an in-memory timeline, reporting each failure
/// with its own remedy.
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
        // A carriage-return counter is only a counter on a terminal; piped
        // into a log it is a run-on line of repeats, so it is shown there
        // alone.
        let live_progress = !json && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut last_tick = None::<Duration>;
        let progress = |elapsed: Duration, events: usize| {
            if !live_progress {
                return;
            }
            if last_tick.is_some_and(|last| elapsed.saturating_sub(last) < PROGRESS_PERIOD) {
                return;
            }
            last_tick = Some(elapsed);
            // In-place recording progress is terminal control, not a diagnostic.
            eprint!("\rrecording... {}s ({events} events)", elapsed.as_secs());
            let _ = std::io::stderr().flush();
        };

        let recorded =
            phux_client::record::record_terminal(&socket_path, terminal_id, max_duration, progress)
                .await;
        if live_progress {
            // Close the progress line so the completion one-liner does not
            // land on top of it.
            eprintln!();
        }

        match recorded {
            Ok(recording) => into_timeline(recording, idle_limit).map_err(|message| {
                error!("rec failed: {message}");
                ExitCode::FAILURE
            }),
            Err(err @ (AttachError::Io(_) | AttachError::Disconnected)) => {
                Err(json_err::report_no_server(json, &err, &socket_path, "rec"))
            }
            Err(err) => {
                error!("rec failed: {err}");
                Err(ExitCode::FAILURE)
            }
        }
    })
}

/// Record the session composite into `cast_path` at a virtual cell grid.
///
/// The attach votes a zero viewport, so the server resizes no pane. The
/// grid comes from `--size` pixels (default `1920x1080`) floored to the
/// recording face.
fn capture_composited(
    target: Option<&str>,
    socket: Option<PathBuf>,
    cast_path: &Path,
    size: Option<&str>,
    version: CastVersion,
    max_duration: Option<Duration>,
    json: bool,
) -> Result<(CastHeader, Vec<CastEvent>), ExitCode> {
    let (cols, rows) = composite_cells(size, json)?;
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = cli_runtime()?;
    let cast_path = cast_path.to_path_buf();

    rt.block_on(async move {
        let attach_target = composited_attach_target(target, &socket_path, json).await?;

        let live_progress = !json && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut last_tick = None::<Duration>;
        let progress = |elapsed: Duration, events: u64| {
            if !live_progress {
                return;
            }
            if last_tick.is_some_and(|last| elapsed.saturating_sub(last) < PROGRESS_PERIOD) {
                return;
            }
            last_tick = Some(elapsed);
            // In-place recording progress; preserved byte-for-byte.
            eprint!(
                "\rrecording... {}s ({events} events, {cols}x{rows})",
                elapsed.as_secs()
            );
            let _ = std::io::stderr().flush();
        };

        let recorded = phux_tui::attach::record_composited(
            &socket_path,
            attach_target,
            phux_tui::attach::CompositedCapture {
                cast_path: &cast_path,
                cols,
                rows,
                version,
                max_duration,
            },
            progress,
        )
        .await;
        if live_progress {
            eprintln!();
        }

        match recorded {
            Ok(()) => read_cast_file(&cast_path).map_err(|err| {
                error!("rec: cannot read back {}: {err}", cast_path.display());
                ExitCode::FAILURE
            }),
            Err(err @ (AttachError::Io(_) | AttachError::Disconnected))
                if !cast_has_bytes(&cast_path) && dial_failure(&err) =>
            {
                Err(json_err::report_no_server(json, &err, &socket_path, "rec"))
            }
            Err(err) => {
                error!("rec failed: {err}");
                if cast_has_bytes(&cast_path) {
                    hint!("a partial capture is at {}", cast_path.display());
                }
                Err(ExitCode::FAILURE)
            }
        }
    })
}

/// Whether `path` already holds a cast header, so a later error is a
/// partial recording and the dial failure is a missing server.
fn cast_has_bytes(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0)
}

/// Connect failures the pane recorder also reports as "no server".
fn dial_failure(err: &AttachError) -> bool {
    match err {
        AttachError::Disconnected => true,
        AttachError::Io(err) => matches!(
            err.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::NotFound
                | io::ErrorKind::TimedOut
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::NetworkUnreachable
        ),
        _ => false,
    }
}

/// Cell grid for `--size`, or 240x67 when the flag is omitted.
fn composite_cells(size: Option<&str>, json: bool) -> Result<(u16, u16), ExitCode> {
    let (width, height) = match size {
        None => (DEFAULT_COMPOSITE_WIDTH_PX, DEFAULT_COMPOSITE_HEIGHT_PX),
        Some(text) => phux_record::parse_pixel_size(text).ok_or_else(|| {
            json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::IO,
                    format!("--size {text:?} is not WIDTHxHEIGHT pixels"),
                    "pass a size such as 1920x1080; both numbers must be greater than 0",
                ),
                2,
            )
        })?,
    };
    Ok(phux_record::cells_for_pixels(width, height))
}

/// The session a composited recording attaches to. A bare session name (or
/// no TARGET) attaches as written; any other selector resolves to its pane
/// first and attaches that pane's session.
async fn composited_attach_target(
    session: Option<&str>,
    socket_path: &Path,
    json: bool,
) -> Result<phux_protocol::wire::frame::AttachTarget, ExitCode> {
    use phux_client::selector::Selector;
    use phux_protocol::wire::frame::AttachTarget;
    let Some(raw) = session else {
        return Ok(AttachTarget::Last);
    };
    let selector = parse_selector(Some(raw))?;
    if let Selector::Session(name) = selector {
        return Ok(AttachTarget::ByName(name));
    }
    let pane = resolve_target(socket_path, &selector, "rec", json).await?;
    let snapshot = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "rec"))?
        .into_snapshot_ignoring_degradation();
    let Some(name) = session_name_of_pane(&snapshot, &pane) else {
        return Err(json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::NO_SUCH_TARGET,
                format!("{raw} is not in a local session that can be recorded"),
                "pass a session name, or a pane selector in a local session",
            ),
            1,
        ));
    };
    Ok(AttachTarget::ByName(name))
}

/// The name of the session that holds `pane`.
fn session_name_of_pane(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    pane: &phux_protocol::ids::ResourceId,
) -> Option<String> {
    let session = phux_client::spawn::ownership_for_terminal(snapshot, pane)?;
    snapshot
        .sessions
        .iter()
        .find(|candidate| candidate.id == session)
        .map(|session| session.name.clone())
}

/// `phux rec` without `--out`. `phux rec mark` never reaches here.
pub(crate) fn missing_out(json: bool) -> ExitCode {
    json_err::emit(
        json,
        &json_err::CliError::new(
            json_err::codes::IO,
            "rec needs --out PATH to capture or re-render",
            "`phux rec -o demo.cast` records; `phux rec mark LABEL` posts a marker and writes no file",
        ),
        2,
    )
}

/// `phux rec mark LABEL`: append one asciicast marker to every live inbox.
pub(crate) fn run_mark(label: &str, json: bool) -> ExitCode {
    if label.chars().count() > MARK_LABEL_MAX_CHARS {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                format!("marker label is longer than {MARK_LABEL_MAX_CHARS} characters"),
                "shorten the label; an empty label is a legal marker",
            ),
            2,
        );
    }
    if label.contains(['\n', '\r']) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                "a marker label cannot contain a newline",
                "pass a single-line label",
            ),
            2,
        );
    }
    match phux_record::post_mark(label) {
        Ok(0) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                "no live recording to mark",
                "start a recording with `phux rec` or `phux --rec`, then run `phux rec mark` again",
            ),
            1,
        ),
        Ok(recordings) => {
            if json {
                outln!("{}", mark_json(label, recordings));
            } else {
                let noun = if recordings == 1 {
                    "recording"
                } else {
                    "recordings"
                };
                if label.is_empty() {
                    outln!("phux: marked {recordings} {noun}");
                } else {
                    outln!("phux: marked {recordings} {noun} ({label})");
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                format!("could not post a marker: {err}"),
                "check that the mark directory is writable (`PHUX_REC_MARK_DIR` or $XDG_RUNTIME_DIR/phux-rec-marks)",
            ),
            1,
        ),
    }
}

/// `--speed-marked` must sit in `(1, 100]`. `None` leaves timing alone.
fn checked_speed(speed: Option<f64>, json: bool) -> Result<Option<f64>, ExitCode> {
    let Some(speed) = speed else {
        return Ok(None);
    };
    if speed.is_finite() && speed > 1.0 && speed <= 100.0 {
        return Ok(Some(speed));
    }
    Err(json_err::emit(
        json,
        &json_err::CliError::new(
            json_err::codes::IO,
            format!("--speed-marked must be greater than 1 and at most 100 (got {speed})"),
            "pass a factor such as 2, or omit the flag to keep original timing",
        ),
        2,
    ))
}

/// The `phux rec mark --json` result. `recordings` is how many inboxes
/// accepted the label.
fn mark_json(label: &str, recordings: usize) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "label": label,
        "recordings": recordings,
    })
}

/// Project a capture onto a cast header and timeline. Zero dimensions mean
/// the priming snapshot never landed, so they are refused rather than written as
/// an unplayable 0x0 cast.
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

/// Produce `spec.final_path` and clean up the intermediate. A cast is
/// already the artifact; an animation is rendered into a buffer bounded by
/// `max_bytes` and written once (a `BufWriter` sink would lose the error in
/// `Drop`).
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
fn report(outcome: &RecOutcome, json: bool) {
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

/// Map `--idle-limit` onto the clamp: `0` or less (or non-finite) disables
/// it.
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
        RecOutcome, cast_version, checked_speed, composite_cells, human_bytes, human_duration,
        idle_limit, into_timeline, mark_json, parse_resize, rec_json,
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

    #[test]
    fn mark_json_pins_label_and_recording_count() {
        let doc = mark_json("intro", 2);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["label"], "intro");
        assert_eq!(doc["recordings"], 2);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(3));
    }

    #[test]
    fn speed_marked_accepts_an_integer_factor_above_one() {
        assert_eq!(checked_speed(None, false).expect("absent"), None);
        assert_eq!(checked_speed(Some(2.0), false).expect("2x"), Some(2.0));
        assert_eq!(checked_speed(Some(100.0), false).expect("cap"), Some(100.0));
        assert!(checked_speed(Some(1.0), false).is_err());
        assert!(checked_speed(Some(0.5), false).is_err());
        assert!(checked_speed(Some(101.0), false).is_err());
        assert!(checked_speed(Some(f64::NAN), false).is_err());
    }

    #[test]
    fn composite_size_defaults_to_1080p_cells() {
        assert_eq!(composite_cells(None, false).expect("default"), (240, 67));
        assert_eq!(
            composite_cells(Some("1920x1080"), false).expect("explicit"),
            (240, 67)
        );
        assert!(composite_cells(Some("0x1080"), false).is_err());
        assert!(composite_cells(Some("wide"), false).is_err());
    }
}
