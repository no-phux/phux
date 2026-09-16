//! Wire primitives for `phux take` / `phux give` / `phux signal` — the
//! supervisory verbs (ADR-0033, "take the wheel + kill").
//!
//! Each resolves a selector client-side to one pane (not this module's job;
//! see [`crate::selector`]) and issues a single control command:
//! `ACQUIRE_INPUT` (seize the input lease), `RELEASE_INPUT`, or
//! `SIGNAL_TERMINAL`.

use std::path::Path;

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::frame::{Command, CommandResult, InputMode, TerminalSignal};

use crate::attach::connection::Connection;
use crate::kill::KeyedError;
use crate::state::Degradation;
/// Whether delivering `signal` is dangerous (ADR-0128): the catalog's
/// payload rule for `SIGNAL_TERMINAL`, so `freeze` and `resume`, the
/// reversible brake, are not.
#[must_use]
pub fn is_dangerous(signal: TerminalSignal) -> bool {
    phux_protocol::kinds::command_is_dangerous(&Command::SignalTerminal {
        terminal_id: ResourceId::local(1),
        signal,
        operation_id: None,
    })
}

/// What an `ACQUIRE_INPUT` / `RELEASE_INPUT` / `SIGNAL_TERMINAL` request
/// answered.
#[derive(Debug, Clone, PartialEq)]
pub enum LeaseOutcome {
    /// The server acknowledged the command.
    Ok,
    /// The server refused.
    Refused(String),
    /// An unexpected reply shape.
    Unexpected(CommandResult),
}

impl LeaseOutcome {
    /// Classify the `COMMAND_RESULT` answering one of this module's commands.
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::Ok => Self::Ok,
            CommandResult::Error { message, .. } => Self::Refused(message),
            other => Self::Unexpected(other),
        }
    }
}

/// The `ACQUIRE_INPUT { Seize }` command that seizes `terminal_id`'s input
/// lease (`phux take`): preempts any current holder.
///
/// `ttl_ms` is ADR-0033's lease TTL — `0` means "held until released or the
/// connection drops" (today's default); any other value arms the
/// server-side expiry timer (`phux take --ttl SECS`).
#[must_use]
pub const fn take_command(terminal_id: ResourceId, ttl_ms: u32) -> Command {
    Command::AcquireInput {
        terminal_id,
        mode: InputMode::Seize,
        ttl_ms,
    }
}

/// The `RELEASE_INPUT` command that returns `terminal_id` to open input
/// (`phux give`): idempotent.
#[must_use]
pub const fn give_command(terminal_id: ResourceId) -> Command {
    Command::ReleaseInput { terminal_id }
}

/// The `SIGNAL_TERMINAL` command that delivers `signal` to `terminal_id`'s
/// process group (`phux signal`).
#[must_use]
pub const fn signal_command(terminal_id: ResourceId, signal: TerminalSignal) -> Command {
    Command::SignalTerminal {
        terminal_id,
        signal,
        operation_id: None,
    }
}

/// Deliver `signal` to `terminal_id` under an idempotency key (`phux signal
/// --idempotency-key`).
///
/// A repeat with the same key answers the first result and delivers nothing
/// (`docs/spec/L1.md` §5.1.1).
///
/// # Errors
///
/// [`KeyedError::Unsupported`], with nothing sent, when the server does not
/// advertise `KEYED_SIGNAL`; transport and decode failures otherwise.
pub async fn signal_keyed(
    conn: &mut Connection,
    request_id: u32,
    terminal_id: ResourceId,
    signal: TerminalSignal,
    operation_id: IdempotencyKey,
) -> Result<(LeaseOutcome, Degradation), KeyedError> {
    if !crate::kill::keyed_signal_supported(conn) {
        return Err(KeyedError::Unsupported);
    }
    let command = Command::SignalTerminal {
        terminal_id,
        signal,
        operation_id: Some(operation_id),
    };
    let (result, interleaved) = conn.request(request_id, command).await?.into_parts();
    Ok((
        LeaseOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

/// [`signal_keyed`] over a fresh connection to the server at `socket_path`.
///
/// # Errors
///
/// As [`signal_keyed`], plus the connect failure.
pub async fn signal_keyed_at(
    socket_path: &Path,
    terminal_id: ResourceId,
    signal: TerminalSignal,
    operation_id: IdempotencyKey,
) -> Result<(LeaseOutcome, Degradation), KeyedError> {
    let mut conn = Connection::connect(socket_path).await?;
    signal_keyed(&mut conn, 1, terminal_id, signal, operation_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::ErrorCode;

    #[test]
    fn commands_carry_the_right_shape() {
        let id = ResourceId::local(3);
        assert_eq!(
            take_command(id.clone(), 0),
            Command::AcquireInput {
                terminal_id: id.clone(),
                mode: InputMode::Seize,
                ttl_ms: 0,
            }
        );
        assert_eq!(
            take_command(id.clone(), 30_000),
            Command::AcquireInput {
                terminal_id: id.clone(),
                mode: InputMode::Seize,
                ttl_ms: 30_000,
            }
        );
        assert_eq!(
            give_command(id.clone()),
            Command::ReleaseInput {
                terminal_id: id.clone()
            }
        );
        assert_eq!(
            signal_command(id.clone(), TerminalSignal::Interrupt),
            Command::SignalTerminal {
                terminal_id: id,
                signal: TerminalSignal::Interrupt,
                operation_id: None,
            }
        );
    }

    #[test]
    fn classifies_every_reply_shape() {
        assert_eq!(
            LeaseOutcome::from_result(CommandResult::Ok),
            LeaseOutcome::Ok
        );
        assert_eq!(
            LeaseOutcome::from_result(CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message: "busy".to_owned(),
            }),
            LeaseOutcome::Refused("busy".to_owned())
        );
        assert!(matches!(
            LeaseOutcome::from_result(CommandResult::OkWith(
                phux_protocol::wire::frame::CommandValue::Json("x".to_owned())
            )),
            LeaseOutcome::Unexpected(_)
        ));
    }
}
