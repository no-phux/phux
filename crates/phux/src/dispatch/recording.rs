//! The dispatch bodies of `phux rec` and `phux play`.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands;

pub(super) fn rec(args: commands::RecArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::RecArgs {
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
    if let Some(commands::rec::RecAction::Mark {
        label,
        json: mark_json,
    }) = action
    {
        return commands::rec::run_mark(&label, json.json || mark_json.json);
    }
    let Some(out) = out else {
        return commands::rec::missing_out(json.json);
    };
    commands::rec::run_rec(commands::rec::RecArgs {
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
