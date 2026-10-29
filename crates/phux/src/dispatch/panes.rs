//! The dispatch bodies of the `Panes` and pane-placing verbs.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands;

pub(super) fn spawn(args: commands::SpawnArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::SpawnArgs {
        satellite,
        target,
        split,
        ratio,
        projection,
        cwd,
        retain,
        idempotency_key,
        json,
        command,
    } = args;
    match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json.json) {
        Ok(idempotency_key) => commands::spawn::run_spawn(
            satellite,
            target,
            split,
            ratio,
            projection.as_deref(),
            cwd,
            json.json,
            socket,
            command,
            commands::spawn::SpawnDurability {
                retain_secs: retain,
                idempotency_key,
            },
        ),
        Err(code) => code,
    }
}

pub(super) fn launch(args: commands::LaunchArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::LaunchArgs {
        integration,
        list,
        print,
        acp,
        session,
        json,
        target,
        split,
        ratio,
        projection,
        cwd,
        extra,
    } = args;
    commands::launch::run_launch(
        integration,
        list,
        print,
        if acp {
            phux_plugin::LaunchMode::Native
        } else {
            phux_plugin::LaunchMode::Terminal
        },
        session.as_deref(),
        json.json,
        target,
        split,
        ratio,
        projection.as_deref(),
        cwd,
        socket,
        &extra,
    )
}

pub(super) fn insert_pane(args: commands::InsertPaneArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::InsertPaneArgs {
        target,
        new_pane,
        split,
        ratio,
        projection,
        json,
    } = args;
    commands::spatial::run_insert_pane(
        &target,
        &new_pane,
        split.into(),
        ratio,
        projection,
        json,
        socket,
    )
}

pub(super) fn mv(args: commands::MvArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::MvArgs {
        source,
        dest,
        split,
        ratio,
        projection,
        json,
    } = args;
    commands::spatial::run_mv(
        &source,
        &dest,
        split.into(),
        ratio,
        projection,
        json,
        socket,
    )
}

pub(super) fn swap_pane(args: commands::SwapPaneArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::SwapPaneArgs {
        first,
        second,
        projection,
        json,
    } = args;
    commands::spatial::run_swap_pane(&first, &second, projection, json, socket)
}

pub(super) fn signal(args: commands::SignalArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::SignalArgs {
        target,
        signal,
        idempotency_key,
        yes,
        json,
    } = args;
    match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json.json) {
        Ok(key) => commands::supervise::run_signal(&target, signal, key, yes, json.json, socket),
        Err(code) => code,
    }
}

pub(super) fn snapshot(args: commands::SnapshotArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::SnapshotArgs {
        session,
        json,
        scrollback,
        cells,
        tail,
        unwrap,
        rendered,
        format,
        cols,
        rows,
    } = args;
    commands::snapshot::run_snapshot(
        session.as_deref(),
        json.json,
        &commands::snapshot::ReadOpts {
            scrollback,
            cells,
            tail,
            unwrap,
            format,
        },
        &commands::snapshot::RenderedOpts {
            rendered,
            cols,
            rows,
        },
        socket,
    )
}

pub(super) fn upload(args: commands::UploadArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::UploadArgs {
        target,
        file,
        extension,
        timeout,
        json,
        remote,
    } = args;
    commands::upload::run(
        &target,
        file,
        extension,
        timeout,
        json,
        commands::server_target::ServerSpec {
            socket,
            remote: remote.remote,
        },
    )
}

pub(super) fn wait(args: commands::WaitArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::WaitArgs {
        session,
        until,
        regex,
        tail,
        output_only,
        idle,
        timeout,
        json,
    } = args;
    commands::wait::run_wait(commands::wait::WaitArgs {
        session: session.as_deref(),
        until,
        regex,
        idle,
        tail,
        output_only,
        timeout,
        json: json.json,
        socket,
    })
}

pub(super) fn watch(args: commands::WatchArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::WatchArgs {
        session,
        until,
        timeout,
        after,
        json,
    } = args;
    commands::watch::run_watch(commands::watch::WatchArgs {
        session: session.as_deref(),
        until: &until,
        timeout,
        after: after.as_deref(),
        json: json.json,
        socket,
    })
}

pub(super) fn ask(args: commands::AskArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::AskArgs {
        target,
        id,
        suggestions,
        elapsed_seconds,
        json,
        question,
    } = args;
    commands::ask::run_ask(
        &target,
        id,
        suggestions,
        elapsed_seconds,
        json.json,
        question,
        socket,
    )
}

pub(super) fn run(args: commands::RunArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::RunArgs {
        target,
        command,
        timeout,
        force,
        json,
    } = args;
    commands::run::run_run(&target, &command, timeout, force, json.json, socket)
}
