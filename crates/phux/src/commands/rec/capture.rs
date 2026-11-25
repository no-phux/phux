//! Live pane and composited-session capture, including progress and bootstrap validation.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::read::Verb;
use phux_config::socket::default_socket_path;
use phux_record::cast::{CastEvent, CastHeader, CastVersion};

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_terminal_target};

use super::attach_target::composited_attach_target;
use super::output::read_cast_file;

/// How often the headless capture repaints its stderr progress line.
const PROGRESS_PERIOD: Duration = Duration::from_millis(250);

/// Pixel size of a composited capture when `--size` is omitted: 1080p.
const DEFAULT_COMPOSITE_WIDTH_PX: u32 = 1920;
const DEFAULT_COMPOSITE_HEIGHT_PX: u32 = 1080;
/// Capture a live pane into an in-memory timeline, reporting each failure
/// with its own remedy.
pub(super) fn capture(
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
        let terminal_id = resolve_terminal_target(&socket_path, &selector, Verb::Rec)
            .await
            .map_err(|refusal| refusal.emit(json))?;

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
pub(super) fn capture_composited(
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
pub(super) fn composite_cells(size: Option<&str>, json: bool) -> Result<(u16, u16), ExitCode> {
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
/// Project a capture onto a cast header and timeline. Zero dimensions mean
/// the priming snapshot never landed, so they are refused rather than written as
/// an unplayable 0x0 cast.
pub(super) fn into_timeline(
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
