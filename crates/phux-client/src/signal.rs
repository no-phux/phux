//! Wire primitives for `phux take` / `phux give` / `phux signal` — the
//! supervisory verbs (ADR-0033, "take the wheel + kill").
//!
//! Each resolves a selector client-side to one pane (not this module's job;
//! see [`crate::selector`]) and issues a single control command:
//! `ACQUIRE_INPUT` (seize the input lease), `RELEASE_INPUT`, or
//! `SIGNAL_TERMINAL`.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, InputMode, TerminalSignal};

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
    }
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
