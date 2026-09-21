//! Wire primitives for `phux take` / `phux give` / `phux signal` — the
//! supervisory verbs (ADR-0033, "take the wheel + kill").
//!
//! `take` / `give` still issue a single control command after the caller
//! resolves a selector. [`deliver`] is the resolve-and-send path both
//! `phux signal` and MCP `phux_signal` call, so those surfaces cannot drift.

use std::path::Path;

use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::wire::frame::{Command, CommandResult, InputMode, TerminalSignal};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::kill::KeyedError;
use crate::selector::{self, Selector};
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

/// Why [`deliver`] did not signal the target.
#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    /// `%name` refused (ADR-0075).
    #[error(transparent)]
    Agent(#[from] crate::selector::AgentResolveError),
    /// The selector matched no pane.
    #[error("no such target")]
    Miss {
        /// What the snapshot could not see; empty means a genuine miss.
        degradation: Degradation,
    },
    /// Connect, keyed-signal, or transport failure.
    #[error(transparent)]
    Keyed(#[from] KeyedError),
}

impl From<AttachError> for SignalError {
    fn from(err: AttachError) -> Self {
        Self::Keyed(KeyedError::Attach(err))
    }
}

/// A [`deliver`] that reached the server.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivered {
    /// What `SIGNAL_TERMINAL` answered.
    pub outcome: LeaseOutcome,
    /// Interleaved notices from the signal round trip.
    pub interleaved: Vec<String>,
}

/// Resolve `selector` and deliver `signal` to the one pane it names.
///
/// An input-verb resolution, including the withdrawn `%name` guard.
/// Snapshot partial-view notices for a hit are appended to `notices`
/// before the signal is sent.
///
/// # Errors
///
/// [`SignalError`] — see its variants.
pub async fn deliver(
    socket: &Path,
    selector: &Selector,
    signal: TerminalSignal,
    key: Option<IdempotencyKey>,
    notices: &mut Vec<String>,
) -> Result<Delivered, SignalError> {
    let mut conn = Connection::connect(socket).await?;
    let (snapshot, degradation) = crate::state::get_state_on(&mut conn).await?.into_parts();
    let terminal = resolve_one_for_input(&mut conn, selector, &snapshot, &degradation).await?;
    notices.extend(degradation.notices().iter().cloned());
    let (outcome, interleaved) = match key {
        None => {
            let command = signal_command(terminal, signal);
            let (result, interleaved) = conn.request(1, command).await?.into_parts();
            (
                LeaseOutcome::from_result(result),
                Degradation::from_interleaved(&interleaved),
            )
        }
        Some(key) => signal_keyed(&mut conn, 1, terminal, signal, key).await?,
    };
    drop(conn);
    Ok(Delivered {
        outcome,
        interleaved: interleaved.notices().to_vec(),
    })
}

/// The one pane an input verb addresses. `%name` never travels
/// [`selector::pick_target_pane`] (ADR-0075 point 3).
async fn resolve_one_for_input(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    degradation: &Degradation,
) -> Result<ResourceId, SignalError> {
    if let Selector::Agent(name) = selector {
        let index = crate::state::fetch_agent_index(conn, snapshot).await;
        return Ok(selector::resolve_agent_for_input(name, snapshot, &index)?.terminal);
    }
    let candidates = if matches!(selector, Selector::Tag(_)) {
        let index = crate::state::fetch_tag_index(conn, snapshot).await;
        selector::resolve_with_tags(selector, snapshot, &index)
    } else {
        selector::resolve(selector, snapshot)
    };
    selector::pick_target_pane(&candidates, &snapshot.focused_resource).ok_or_else(|| {
        SignalError::Miss {
            degradation: degradation.clone(),
        }
    })
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

    async fn deliver_on(
        spec: crate::testkit::ScriptSpec,
        target: &str,
    ) -> (
        Result<Delivered, SignalError>,
        Vec<String>,
        Vec<phux_protocol::wire::frame::FrameKind>,
    ) {
        use crate::testkit::ScriptedServer;
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let selector = crate::selector::parse(target).expect("selector");
        let mut notices = Vec::new();
        let result = deliver(
            &socket,
            &selector,
            TerminalSignal::Freeze,
            None,
            &mut notices,
        )
        .await;
        (result, notices, server.await.expect("scripted server task"))
    }

    #[tokio::test]
    async fn deliver_signals_a_resolved_pane_and_warns_on_a_partial_hit() {
        use crate::testkit::ScriptSpec;
        use phux_protocol::ids::{SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

        const NOTICE: &str = "satellite build-box is unreachable: link is down";
        let session = SessionId::new(1);
        let window = WindowId::new(10);
        let snapshot = SessionSnapshot::new(session, window, ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(session, "work").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, session, "shell")])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(1),
                window,
                80,
                24,
            )]);

        let (result, notices, seen) =
            deliver_on(ScriptSpec::new().state(snapshot.clone()), "@1").await;
        let delivered = result.expect("signal");
        assert_eq!(delivered.outcome, LeaseOutcome::Ok);
        assert!(notices.is_empty());
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                phux_protocol::wire::frame::FrameKind::Command {
                    command: Command::SignalTerminal {
                        signal: TerminalSignal::Freeze,
                        ..
                    },
                    ..
                }
            )),
            "expected SIGNAL_TERMINAL freeze; sent {seen:?}"
        );

        let (result, notices, _) = deliver_on(
            ScriptSpec::new()
                .state(snapshot.clone())
                .degradation_notice(NOTICE),
            "@1",
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(notices, [NOTICE]);

        let (result, notices, _) = deliver_on(
            ScriptSpec::new().state(snapshot).degradation_notice(NOTICE),
            "@9",
        )
        .await;
        assert!(
            matches!(
                result,
                Err(SignalError::Miss { ref degradation }) if degradation.notices() == [NOTICE]
            ),
            "{result:?}"
        );
        assert!(notices.is_empty());
    }
}
