//! `phux rec` — capture a pane and export it as a cast, a GIF, or an APNG
//! (ADR-0060). [`run_rec`] is the headless verb (a pure observer via
//! `phux_client::record`, or a zero-vote composited attach); [`finalize`] is
//! the tail of the interactive `--rec` path, whose cast is already on disk.
//! Both go through [`spec::plan`]. [`mark::run_mark`] posts an asciicast `m` event
//! to every recording in progress.
//!
//! stdout carries only the completion line (or one JSON object under
//! `--json`); diagnostics go to stderr, and only on the headless path, since
//! `--rec` attach owns the alt screen until it exits.

pub(crate) mod spec;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_record::cast::CastVersion;
use phux_record::render::{OutputFormat, RenderOptions};
use phux_record::timeline::clamp_idle;
use usage::Subcommands;

use crate::commands::{RecFormat, json_err};

pub(crate) use spec::RecordSpec;

mod attach_target;
mod capture;
mod output;
use capture::{capture, capture_composited};
pub(crate) use output::finalize;
use output::{export, read_cast_file, report, report_export_failure, write_cast_file};
mod mark;
use mark::run_mark;

/// Subcommands of `phux rec`. Absent, the verb captures or re-renders.
#[derive(Debug, Subcommands)]
pub(crate) enum RecAction {
    /// Post an asciicast marker to every recording in progress
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

/// Run `phux rec` as parsed: `rec mark` posts a marker; anything else
/// records, which needs `-o`.
pub(crate) fn run(args: crate::commands::RecArgs, socket: Option<std::path::PathBuf>) -> ExitCode {
    let crate::commands::RecArgs {
        action,
        target,
        out,
        format,
        from,
        duration,
        fps,
        idle_limit,
        max_bytes,
        cast_version,
        composited,
        size,
        speed_marked,
        marker_labels,
        json,
    } = args;
    if let Some(RecAction::Mark {
        label,
        json: mark_json,
    }) = action
    {
        return run_mark(&label, json.json || mark_json.json);
    }
    let Some(out) = out else {
        return missing_out(json.json);
    };
    run_rec(RecArgs {
        target: target.as_deref(),
        out: &out,
        format,
        from: from.as_deref(),
        duration,
        fps,
        idle_limit,
        max_bytes,
        cast_version,
        json: json.json,
        socket,
        composited,
        size: size.as_deref(),
        speed_marked,
        marker_labels,
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests;
