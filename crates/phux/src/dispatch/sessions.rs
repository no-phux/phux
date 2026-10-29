//! The dispatch bodies of the `Sessions` verbs.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::commands;

pub(super) fn ls(args: commands::LsArgs, socket: Option<PathBuf>) -> ExitCode {
    match args {
        commands::LsArgs {
            json, all: true, ..
        } => commands::ls::run_ls_all(json.json, socket),
        commands::LsArgs {
            json,
            remote,
            session,
            detail,
            ..
        } => commands::ls::run_ls(
            json.json,
            &commands::ls::Depth::of(session, detail),
            remote.with_socket(socket),
        ),
    }
}

pub(super) fn new(args: commands::NewArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::NewArgs {
        name,
        session,
        cwd,
        json,
        env,
        idempotency_key,
        remote,
        empty,
        ephemeral,
        command,
    } = args;
    match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json) {
        Ok(idempotency_key) => commands::new::run_new(
            name,
            session,
            cwd,
            remote.with_socket(socket),
            commands::new::NewMode {
                json,
                empty,
                ephemeral,
                idempotency_key,
            },
            command,
            env.into_iter().map(|item| (item.key, item.value)).collect(),
        ),
        Err(code) => code,
    }
}

pub(super) fn kill(args: commands::KillArgs, socket: Option<PathBuf>) -> ExitCode {
    let commands::KillArgs {
        target,
        server,
        idempotency_key,
        yes,
        json,
        remote,
    } = args;
    match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json) {
        Ok(key) => commands::kill::run(target, server, key, yes, json, remote.with_socket(socket)),
        Err(code) => code,
    }
}

pub(super) fn organize(args: commands::OrganizeArgs, socket: Option<PathBuf>) -> ExitCode {
    match args {
        commands::OrganizeArgs {
            undo: true,
            session,
            json,
            ..
        } => commands::organize::run_undo(session.as_deref(), json, socket),
        commands::OrganizeArgs {
            session,
            dry_run,
            offline,
            min_confidence,
            undo: false,
            json,
        } => commands::organize::run(
            &commands::organize::Request {
                session,
                dry_run,
                offline,
                min_confidence,
                json,
            },
            socket,
        ),
    }
}
