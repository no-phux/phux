//! The usage refusals the grammar cannot express, checked after parsing.

use std::process::ExitCode;

use crate::commands::{self, Command};
use crate::{Cli, json_output_requested};

/// The teaching error for a root `--rec` in front of a verb. Post-parse
/// because the root global `--socket` rules out
/// `args_conflicts_with_subcommands`.
pub(crate) const fn root_rec_before_verb(cli: &Cli) -> Option<&'static str> {
    if cli.command.is_some() && (cli.rec.rec.is_some() || cli.rec.rec_format.is_some()) {
        Some(
            "a root `--rec` belongs to the naked `phux` attach alone; \
             use `phux attach --rec PATH` to record an attach, or \
             `phux rec TARGET -o PATH` for headless capture",
        )
    } else {
        None
    }
}

/// `--qr` and `--no-qr` belong to minting a credential. usage-rs lets parent
/// flags parse next to a subcommand, so the refusal is post-parse.
pub(crate) const fn pair_qr_with_action(cli: &Cli) -> Option<&'static str> {
    match &cli.command {
        Some(Command::Pair(commands::PairArgs {
            action: Some(_),
            qr: true,
            ..
        })) => Some(
            "--qr belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        Some(Command::Pair(commands::PairArgs {
            action: Some(_),
            secret_file: Some(_),
            ..
        })) => Some(
            "--secret-file belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        Some(Command::Pair(commands::PairArgs {
            action: Some(_),
            expires: Some(_),
            ..
        })) => Some(
            "--expires belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        Some(Command::Pair(commands::PairArgs {
            action: Some(_),
            no_qr: true,
            ..
        })) => Some(
            "--no-qr belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        Some(Command::Pair(commands::PairArgs {
            action: Some(_),
            relay_route: Some(_),
            ..
        })) => Some(
            "--relay-route belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        _ => None,
    }
}

/// The teaching error for a root `--remote` in front of a verb (same scope
/// rule as the root `--rec`).
pub(crate) const fn root_remote_before_verb(cli: &Cli) -> Option<&'static str> {
    if cli.command.is_some() && cli.remote.is_some() {
        Some(
            "a root `--remote` belongs to the naked `phux` attach alone; \
             place it after the verb instead (`phux ls --remote HOST`; `ls`, `new`, `kill`, \
             `rename`, and `detach` take it), or use `phux attach --remote HOST` to name a \
             session, a `--code`, or `--no-enroll`",
        )
    } else {
        None
    }
}

/// The parse error for this invocation's `--remote`, root or verb-scoped;
/// checked before the TTY preflight.
fn malformed_remote_target(cli: &Cli) -> Option<String> {
    commands::remote_target::parse_target(invocation_remote(cli)?).err()
}

/// The `--remote` this invocation carries: the root copy for the naked
/// attach, otherwise the verb's own (see [`commands::verb_remote`]).
fn invocation_remote(cli: &Cli) -> Option<&str> {
    cli.command
        .as_ref()
        .map_or(cli.remote.as_deref(), commands::verb_remote)
}

/// Whether this invocation pairs the local-UDS `--socket` with the network
/// `--remote`. Post-parse because conflicts are validated per parser and
/// `--socket` is a root global.
fn socket_and_remote_collide(cli: &Cli) -> bool {
    cli.socket.is_some() && invocation_remote(cli).is_some()
}

/// The usage refusals the grammar cannot express, checked after parsing and
/// before any global setup: `--capabilities` without `--json`, the root
/// `--rec`/`--remote` scope rules, a malformed `--remote`, `--socket` with
/// `--remote`, and `--socket` on a verb that never dials. Each exits 2.
pub(crate) fn usage_refusal(cli: &Cli) -> Option<ExitCode> {
    if cli.capabilities {
        return Some(emit_usage_error(
            "--capabilities requires --json".to_owned(),
        ));
    }

    if matches!(
        &cli.command,
        Some(Command::Server(commands::ServerArgs {
            ensure: false,
            json: commands::JsonOpt { json: true },
            ..
        }))
    ) {
        return Some(emit_usage_error(
            "`phux server --json` requires `--ensure`".to_owned(),
        ));
    }

    if let Some(message) = root_rec_before_verb(cli) {
        return Some(emit_usage_error(message.to_owned()));
    }
    if let Some(message) = root_remote_before_verb(cli) {
        return Some(emit_usage_error(message.to_owned()));
    }
    if let Some(message) = pair_qr_with_action(cli) {
        return Some(emit_usage_error(message.to_owned()));
    }
    // A malformed `--remote` target is a usage error, so it is reported here
    // — before the interactive TTY preflight below. Otherwise a typo in a
    // script would surface as "interactive use requires a terminal", which
    // names the wrong problem entirely.
    if let Some(message) = malformed_remote_target(cli) {
        return Some(emit_usage_error(message));
    }
    if socket_and_remote_collide(cli) {
        return Some(emit_usage_error(
            commands::server_target::SOCKET_REMOTE_CONFLICT.to_owned(),
        ));
    }
    if cli.socket.is_some()
        && let Some(verb) = cli.command.as_ref().and_then(commands::socketless_verb)
    {
        return Some(emit_usage_error(format!(
            "`phux {verb}` never dials a server, so --socket has no effect here; drop it"
        )));
    }

    None
}

pub(crate) fn emit_usage_error(message: String) -> ExitCode {
    commands::json_err::emit(
        json_output_requested(),
        &commands::json_err::CliError::new(
            commands::json_err::codes::USAGE_ERROR,
            message,
            "run `phux --help` for usage",
        ),
        2,
    )
}
