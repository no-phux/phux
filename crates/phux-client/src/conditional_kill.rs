//! Conditional kills (ADR-0109, `docs/spec/L1.md` §5.2.1).
//!
//! A spawn can ask to be bound to the instance token of the id space it was
//! allocated from ([`request_binding`]); a later [`BoundResource::kill_command`]
//! kills it only if that id space is unchanged and no other connection has
//! attached it, which the server checks atomically. The TUI's stray-pane
//! retry builds its frames from these pieces.

use phux_protocol::ids::{ResourceId, ServerInstance};
use phux_protocol::wire::frame::{Command, FrameKind, KillPrecondition};

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
    /// The `KILL_RESOURCE_IF` that kills this resource only if its id space
    /// is unchanged, no connection but the spawning one attached or used it,
    /// and it has no child resource.
    #[must_use]
    pub fn kill_command(&self) -> Command {
        Command::KillResourceIf {
            terminal_id: self.id.clone(),
            precondition: KillPrecondition::spawned_and_unattached(self.instance),
            operation_id: None,
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

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{GroupId, ResourceId, SatelliteHost, ServerInstance};
    use phux_protocol::wire::frame::{Command, FrameKind, KillConditions};

    use super::{BoundResource, request_binding};

    fn edge_pane() -> ResourceId {
        ResourceId::satellite(SatelliteHost::new("edge"), 9)
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
            ..
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
}
