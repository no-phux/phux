//! Conditional kills (ADR-0109, `docs/spec/L1.md` §5.2.1).
//!
//! Bind a spawned resource to the id space it was allocated from, and later
//! kill it only if that id space is unchanged and no other connection has
//! attached it.
//!
//! A client that cleans up after itself late (a stray satellite pane killed
//! once its link recovers) cannot tell from its own state whether the pane
//! is still safe to kill: another client may have attached it, or the
//! satellite may have restarted and reissued the id to someone else's pane.
//! The server can tell, and checks both atomically. The flow:
//!
//! 1. Ask for binding on the spawn ([`request_binding`]). A server with
//!    [`ServerFeature::ConditionalKill`] answers `SpawnResult::OkBound`;
//!    through a hub, the token is the satellite's.
//! 2. Keep the [`BoundResource`] ([`BoundResource::from_spawn`]).
//! 3. Later, send [`BoundResource::kill_command`] (or [`kill_if`]) and read
//!    the reply with [`KillIfOutcome::from_result`].
//!
//! Nothing here keeps state. The TUI's stray-pane retry (phux-c2td.23)
//! builds its frames from these pieces on its own connection.

use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ResourceId, ServerInstance};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, KillPrecondition, SpawnResult,
};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// A spawned resource bound to the instance token of the server that
/// allocated its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundResource {
    /// The resource, as the spawn reply named it (satellite-tagged through a
    /// hub).
    pub id: ResourceId,
    /// The token naming the id space `id` came from.
    pub instance: ServerInstance,
}

impl BoundResource {
    /// The bound resource a spawn reply names; `None` for a refusal or an
    /// unbound `Ok` (the server did not bind it, so it cannot be killed
    /// conditionally).
    #[must_use]
    pub fn from_spawn(result: &SpawnResult) -> Option<Self> {
        let instance = result.instance()?;
        let id = result.spawned_id()?.clone();
        Some(Self { id, instance })
    }

    /// The `KILL_RESOURCE_IF` that kills this resource only if its id space
    /// is unchanged, no connection but the spawning one attached or used it,
    /// and it has no child resource.
    #[must_use]
    pub fn kill_command(&self) -> Command {
        Command::KillResourceIf {
            terminal_id: self.id.clone(),
            precondition: KillPrecondition::spawned_and_unattached(self.instance),
        }
    }
}

/// Mark a `SPAWN_RESOURCE` frame as asking for binding.
///
/// Returns `false`, leaving `frame` untouched, when it is not a spawn. Safe
/// to send to a server without the feature: it skips the field and answers
/// an unbound `Ok`.
pub fn request_binding(frame: &mut FrameKind) -> bool {
    let FrameKind::SpawnResource { resource, .. } = frame else {
        return false;
    };
    resource.get_or_insert_with(Box::default).bind_instance = true;
    true
}

/// Whether the server behind `conn` evaluates conditional kills. `false` on
/// the unnegotiated test seam.
#[must_use]
pub fn supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::ConditionalKill)
    })
}

/// What a conditional kill did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillIfOutcome {
    /// Every precondition held; the resource is being torn down.
    Killed,
    /// A precondition failed, or a hub could not vouch for it; nothing was
    /// killed. The resource is someone else's now, or no longer known:
    /// leave it alone.
    Refused(String),
    /// The id names nothing in the server's current id space: already gone.
    Gone,
    /// The server does not advertise `CONDITIONAL_KILL`; nothing was sent.
    Unsupported,
    /// Any other refusal (an unreachable satellite, a saturated link). The
    /// resource was not killed; a later retry may succeed.
    Failed {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
}

impl KillIfOutcome {
    /// Classify the `COMMAND_RESULT` answering a `KILL_RESOURCE_IF`.
    #[must_use]
    pub fn from_result(result: &CommandResult) -> Self {
        match result {
            CommandResult::Ok | CommandResult::OkWith(_) => Self::Killed,
            CommandResult::Error { code, message } => Self::from_error(*code, message),
            other => Self::Failed {
                code: ErrorCode::InternalError,
                message: format!("unrecognised KILL_RESOURCE_IF result: {other:?}"),
            },
        }
    }

    /// Classify a refusal by its code.
    fn from_error(code: ErrorCode, message: &str) -> Self {
        match code {
            ErrorCode::PreconditionFailed => Self::Refused(message.to_owned()),
            ErrorCode::TerminalNotFound => Self::Gone,
            code => Self::Failed {
                code,
                message: message.to_owned(),
            },
        }
    }
}

/// Send `bound`'s conditional kill on `conn` and wait for its answer.
///
/// Sends nothing and answers [`KillIfOutcome::Unsupported`] when the server
/// does not advertise the feature: an older server cannot decode the command.
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn kill_if(
    conn: &mut Connection,
    request_id: u32,
    bound: &BoundResource,
) -> Result<KillIfOutcome, AttachError> {
    if !supported(conn) {
        return Ok(KillIfOutcome::Unsupported);
    }
    let (result, interleaved) = conn
        .request(request_id, bound.kill_command())
        .await?
        .into_parts();
    crate::state::report_degradation(&interleaved);
    Ok(KillIfOutcome::from_result(&result))
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{GroupId, ResourceId, SatelliteHost, ServerInstance};
    use phux_protocol::wire::frame::{
        Command, CommandResult, ErrorCode, FrameKind, KillConditions, SpawnError, SpawnResult,
    };

    use super::{BoundResource, KillIfOutcome, request_binding};

    fn edge_pane() -> ResourceId {
        ResourceId::satellite(SatelliteHost::new("edge"), 9)
    }

    #[test]
    fn only_a_bound_success_yields_a_bound_resource() {
        let instance = ServerInstance::new([4; 16]);
        let bound = SpawnResult::OkBound {
            id: edge_pane(),
            instance,
        };
        assert_eq!(
            BoundResource::from_spawn(&bound),
            Some(BoundResource {
                id: edge_pane(),
                instance
            })
        );
        assert_eq!(
            BoundResource::from_spawn(&SpawnResult::Ok(edge_pane())),
            None
        );
        assert_eq!(
            BoundResource::from_spawn(&SpawnResult::Err(SpawnError::GroupNotFound)),
            None
        );
    }

    #[test]
    fn the_kill_carries_the_token_and_the_attachment_condition() {
        let bound = BoundResource {
            id: edge_pane(),
            instance: ServerInstance::new([4; 16]),
        };
        let Command::KillResourceIf {
            terminal_id,
            precondition,
        } = bound.kill_command()
        else {
            panic!("expected KILL_RESOURCE_IF");
        };
        assert_eq!(terminal_id, edge_pane());
        assert_eq!(precondition.instance, Some(bound.instance));
        assert!(
            precondition
                .conditions
                .contains(KillConditions::UNATTACHED_SINCE_SPAWN)
        );
    }

    #[test]
    fn binding_is_requested_only_on_a_spawn() {
        let mut spawn = FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: Some(SatelliteHost::new("edge")),
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        };
        assert!(request_binding(&mut spawn));
        let FrameKind::SpawnResource { resource, .. } = &spawn else {
            unreachable!();
        };
        assert!(resource.as_ref().is_some_and(|r| r.bind_instance));
        let mut other = FrameKind::Ping { nonce: 1 };
        assert!(!request_binding(&mut other));
    }

    #[test]
    fn replies_classify_by_code() {
        assert_eq!(
            KillIfOutcome::from_result(&CommandResult::Ok),
            KillIfOutcome::Killed
        );
        let refused = CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: "attached".to_owned(),
        };
        assert_eq!(
            KillIfOutcome::from_result(&refused),
            KillIfOutcome::Refused("attached".to_owned())
        );
        let gone = CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: String::new(),
        };
        assert_eq!(KillIfOutcome::from_result(&gone), KillIfOutcome::Gone);
        let down = CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            message: "down".to_owned(),
        };
        assert!(matches!(
            KillIfOutcome::from_result(&down),
            KillIfOutcome::Failed {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
    }
}
