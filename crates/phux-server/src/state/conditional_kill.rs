//! `KILL_RESOURCE_IF` (`docs/spec/L1.md` §5.2.1, ADR-0109): kill one
//! resource only when the caller's preconditions still hold.
//!
//! The check and the kill run in one `&mut ServerState` borrow, which the
//! runtime takes as one acquisition of the state lock, so no attach can land
//! between them. The precondition reads two things this server tracks: the
//! instance token of its id space ([`super::IdSpace::instance`]) and each
//! spawned resource's provenance (the resource table's spawn records, fed by
//! every subscription).

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{CloseReason, KillConditions, KillPrecondition};

use super::ServerState;
use crate::resource::ResourceId;

/// Why a `KILL_RESOURCE_IF` killed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillIfRefusal {
    /// The id names no resource in this server's id space.
    NotFound,
    /// A precondition failed; the text says which, for the wire message.
    Precondition(&'static str),
}

impl ServerState {
    /// Close `terminal` if every condition in `precondition` holds, or say
    /// why not and close nothing.
    ///
    /// # Errors
    ///
    /// [`KillIfRefusal::Precondition`] when a condition fails,
    /// [`KillIfRefusal::NotFound`] when the id names nothing.
    pub fn kill_resource_if(
        &mut self,
        terminal: &WireResourceId,
        precondition: &KillPrecondition,
    ) -> Result<(), KillIfRefusal> {
        let core = self.admit_conditional_kill(terminal, precondition)?;
        self.close_resources(&[core], CloseReason::Killed);
        Ok(())
    }

    /// The resource a conditional kill may close, or why it may not.
    ///
    /// The instance is checked before the id is resolved: under another
    /// token the id may name some other resource, so whether it exists says
    /// nothing about the one the caller meant.
    fn admit_conditional_kill(
        &self,
        terminal: &WireResourceId,
        precondition: &KillPrecondition,
    ) -> Result<ResourceId, KillIfRefusal> {
        if precondition.conditions.unknown_bits() != 0 {
            return Err(KillIfRefusal::Precondition(
                "a condition bit is unknown to this server",
            ));
        }
        if lacks_required_instance(precondition) {
            return Err(KillIfRefusal::Precondition(
                "UNATTACHED_SINCE_SPAWN requires an instance token",
            ));
        }
        if !self.instance_matches(precondition) {
            return Err(KillIfRefusal::Precondition(
                "the instance token no longer names this server's id space",
            ));
        }
        let core = self
            .terminal_from_wire(terminal)
            .ok_or(KillIfRefusal::NotFound)?;
        if let Some(why) = self.attachment_refusal(core, precondition.conditions) {
            return Err(KillIfRefusal::Precondition(why));
        }
        Ok(core)
    }

    /// `true` unless the precondition names an instance other than ours.
    fn instance_matches(&self, precondition: &KillPrecondition) -> bool {
        precondition
            .instance
            .is_none_or(|instance| instance == self.idspace.instance())
    }

    /// Why `UNATTACHED_SINCE_SPAWN` does not hold for `core`, when it is
    /// asked for. A child resource refuses too: the kill would close it
    /// (`CloseReason::ParentClosed`), and nothing checked who uses it.
    fn attachment_refusal(
        &self,
        core: ResourceId,
        conditions: KillConditions,
    ) -> Option<&'static str> {
        if !conditions.contains(KillConditions::UNATTACHED_SINCE_SPAWN) {
            return None;
        }
        if !self.resources.unattached_since_spawn(core) {
            return Some(
                "the resource was not spawned by a client, or a connection other than its spawner has attached or used it",
            );
        }
        if !self.sessions.registry.children(core).is_empty() {
            return Some("the resource has child resources the kill would close unchecked");
        }
        None
    }

    /// Note that `client` named `terminal` in a verb that uses it: input,
    /// the input lease, an upload, a transcription, a signal, or a screen
    /// read (ADR-0109, L1 §5.2.1). For a resource another connection
    /// spawned, that counts as an attach. A satellite-tagged or unknown id
    /// is not this server's and is ignored.
    pub fn note_resource_use(&mut self, terminal: &WireResourceId, client: super::ClientId) {
        if let Some(core) = self.terminal_from_wire(terminal) {
            self.resources.note_use(client, core);
        }
    }
}

/// `UNATTACHED_SINCE_SPAWN` without an instance token: the id could name a
/// resource from an id space the caller never saw, so the condition cannot
/// be established.
fn lacks_required_instance(precondition: &KillPrecondition) -> bool {
    precondition
        .conditions
        .contains(KillConditions::UNATTACHED_SINCE_SPAWN)
        && precondition.instance.is_none()
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::ServerInstance;
    use phux_protocol::wire::frame::{KillConditions, KillPrecondition};

    use super::KillIfRefusal;
    use crate::state::{ClientId, ServerState};

    /// A state holding one registered pane, as `(state, core, wire)`.
    fn state_with_pane() -> (
        ServerState,
        crate::resource::ResourceId,
        phux_protocol::ids::ResourceId,
    ) {
        let mut state = ServerState::new();
        let sid = state.registry_mut().new_session("main".to_owned());
        let wid = state.registry_mut().new_window(sid).expect("window");
        let core = state.registry_mut().new_terminal(wid).expect("terminal");
        let wire = state.intern_terminal_wire(core);
        (state, core, wire)
    }

    fn untouched(state: &ServerState) -> KillPrecondition {
        KillPrecondition::spawned_and_unattached(state.idspace.instance())
    }

    #[test]
    fn a_spawned_pane_only_its_spawner_attached_is_killed() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        state.subscribe_terminal(ClientId(1), core, None);
        let precondition = untouched(&state);
        // The kill is committed; the id is retired later, when the reap
        // that `close_resources` starts runs.
        assert_eq!(state.kill_resource_if(&wire, &precondition), Ok(()));
    }

    #[test]
    fn another_connection_attaching_refuses_even_after_it_detaches() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        state.subscribe_terminal(ClientId(2), core, None);
        state.unsubscribe_terminal(ClientId(2), core);
        let precondition = untouched(&state);
        assert!(matches!(
            state.kill_resource_if(&wire, &precondition),
            Err(KillIfRefusal::Precondition(_))
        ));
        assert_eq!(state.terminal_from_wire(&wire), Some(core), "kept");
    }

    #[test]
    fn a_pane_no_client_spawned_is_never_unattached_since_spawn() {
        let (mut state, core, wire) = state_with_pane();
        let precondition = untouched(&state);
        assert!(matches!(
            state.kill_resource_if(&wire, &precondition),
            Err(KillIfRefusal::Precondition(_))
        ));
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }

    #[test]
    fn a_different_instance_refuses_before_the_id_is_resolved() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        let mut other = *state.idspace.instance().as_bytes();
        other[0] ^= 0xFF;
        let old_token = KillPrecondition::spawned_and_unattached(ServerInstance::new(other));
        assert!(matches!(
            state.kill_resource_if(&wire, &old_token),
            Err(KillIfRefusal::Precondition(_))
        ));
        let absent = phux_protocol::ids::ResourceId::local(999);
        assert!(
            matches!(
                state.kill_resource_if(&absent, &old_token),
                Err(KillIfRefusal::Precondition(_))
            ),
            "a stale token wins over a missing id"
        );
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }

    #[test]
    fn another_connection_using_the_pane_without_attaching_refuses() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        state.note_resource_use(&wire, ClientId(1));
        let precondition = untouched(&state);
        assert!(
            state.resources.unattached_since_spawn(core),
            "the spawner's own use"
        );
        state.note_resource_use(&wire, ClientId(2));
        assert!(matches!(
            state.kill_resource_if(&wire, &precondition),
            Err(KillIfRefusal::Precondition(_))
        ));
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }

    #[test]
    fn the_attachment_condition_requires_an_instance() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        let no_token = KillPrecondition {
            instance: None,
            conditions: KillConditions::UNATTACHED_SINCE_SPAWN,
        };
        assert!(matches!(
            state.kill_resource_if(&wire, &no_token),
            Err(KillIfRefusal::Precondition(_))
        ));
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }

    #[test]
    fn a_pane_with_a_child_resource_is_refused() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        let facet = phux_core::resource::AgentFacet {
            provider: "claude".to_owned(),
            native_id: None,
            state: None,
        };
        state
            .registry_mut()
            .new_agent_session(core, facet)
            .expect("child");
        assert!(matches!(
            state.kill_resource_if(&wire, &untouched(&state)),
            Err(KillIfRefusal::Precondition(_))
        ));
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }

    #[test]
    fn unknown_condition_bits_refuse_and_a_missing_id_is_not_found() {
        let (mut state, core, wire) = state_with_pane();
        state.record_spawn(core, ClientId(1));
        let unknown = KillPrecondition {
            instance: None,
            conditions: KillConditions::from_bits(0x80),
        };
        assert!(matches!(
            state.kill_resource_if(&wire, &unknown),
            Err(KillIfRefusal::Precondition(_))
        ));
        let absent = phux_protocol::ids::ResourceId::local(999);
        assert_eq!(
            state.kill_resource_if(&absent, &untouched(&state)),
            Err(KillIfRefusal::NotFound)
        );
        assert_eq!(state.terminal_from_wire(&wire), Some(core));
    }
}
