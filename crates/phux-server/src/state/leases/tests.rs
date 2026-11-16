use phux_core::PrincipalId;
use phux_protocol::scope::TerminalScopeSet;

use super::*;
use crate::policy::ConnectionGrant;

const HUMAN: ClientId = ClientId(1);
const AGENT: ClientId = ClientId(2);
const OTHER_AGENT: ClientId = ClientId(3);
const PHONE: ClientId = ClientId(4);
const DELEGATE: ClientId = ClientId(5);

fn agent_id(client: ClientId) -> String {
    format!("sha256:{:064x}", client.0)
}

/// One pane, the owner's connection, and two automation credentials
/// holding every verb (so lease precedence, not the grant, decides).
fn world() -> (ServerState, ResourceId) {
    let mut state = ServerState::new();
    let (_, pane) = state.seed_session("work");
    state.set_connection_grant(HUMAN, ConnectionGrant::owner());
    for agent in [AGENT, OTHER_AGENT] {
        let grant = ConnectionGrant::scoped(
            TerminalScopeSet::parse_all(&["*@global"]).unwrap(),
            Some(agent_id(agent)),
        )
        .unwrap();
        state.set_connection_grant(agent, grant);
    }
    (state, pane)
}

/// [`world`] plus a paired phone (a registry credential enrolled for a
/// person) and an agent's delegated credential.
fn world_with_devices() -> (ServerState, ResourceId) {
    let (mut state, pane) = world();
    let every_verb = || TerminalScopeSet::parse_all(&["*@global"]).unwrap();
    let phone = ConnectionGrant::scoped(every_verb(), Some(agent_id(PHONE)))
        .unwrap()
        .with_kind(phux_core::actor::PrincipalKind::Human);
    state.set_connection_grant(PHONE, phone);
    let delegate = ConnectionGrant::delegated(
        every_verb(),
        PrincipalId::Credential("delegated:00000000000000000000000000000005".to_owned()),
        PrincipalId::Credential(agent_id(PHONE)),
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .unwrap();
    state.set_connection_grant(DELEGATE, delegate);
    (state, pane)
}

#[test]
fn a_paired_devices_lease_is_human_and_beats_every_agent() {
    let (mut state, pane) = world_with_devices();
    state.set_input_lease(pane, PHONE);
    for agent in [AGENT, DELEGATE] {
        assert!(
            state.lease_protected_from(pane, agent),
            "{agent:?} may not take a paired device's lease"
        );
    }
    assert!(
        !state.lease_protected_from(pane, HUMAN),
        "one person's takeover from another's device always succeeds"
    );

    for agent in [AGENT, DELEGATE] {
        state.set_input_lease(pane, agent);
        assert!(
            !state.lease_protected_from(pane, PHONE),
            "the device's deliberate takeover beats {agent:?}"
        );
    }
    state.set_input_lease(pane, HUMAN);
    assert!(
        !state.lease_protected_from(pane, PHONE),
        "people never block each other"
    );
}

/// `(epoch, holder principal)` of every journaled `LeaseChanged`.
fn lease_changes(state: &ServerState) -> Vec<(u64, Option<PrincipalId>)> {
    let known = [HUMAN, AGENT, OTHER_AGENT].map(|client| state.actor_for(client).principal);
    state
        .journal
        .retained()
        .filter_map(|entry| match &entry.event {
            AgentEvent::LeaseChanged { epoch, holder, .. } => Some((
                epoch.get(),
                holder.as_ref().map(|holder| {
                    known
                        .iter()
                        .find(|principal| {
                            crate::org_wire::principal_id(principal) == holder.principal()
                        })
                        .cloned()
                        .expect("a known holder")
                }),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn a_human_held_lease_is_protected_from_agents_and_never_from_a_human() {
    let (mut state, pane) = world();
    state.set_input_lease(pane, HUMAN);
    assert!(state.lease_protected_from(pane, AGENT));
    assert!(
        !state.lease_protected_from(pane, HUMAN),
        "the holder itself"
    );

    state.set_input_lease(pane, AGENT);
    assert!(
        !state.lease_protected_from(pane, HUMAN),
        "a human's takeover always wins"
    );
    assert!(
        !state.lease_protected_from(pane, OTHER_AGENT),
        "an agent's lease has no precedence"
    );
}

#[test]
fn every_lease_change_opens_an_epoch_and_journals_its_holder() {
    let (mut state, pane) = world();
    assert_eq!(state.input_lease_epoch(pane).get(), 0);

    state.set_input_lease(pane, AGENT);
    state.set_input_lease(pane, AGENT);
    assert_eq!(
        state.input_lease_epoch(pane).get(),
        1,
        "no change, no epoch"
    );
    state.set_input_lease(pane, HUMAN);
    assert!(state.release_input_lease(pane, HUMAN));
    state.set_input_lease(pane, AGENT);
    state.detach(AGENT);
    state.set_input_lease(pane, OTHER_AGENT);
    assert_eq!(state.expire_input_lease(pane), Some(OTHER_AGENT));

    let agent = PrincipalId::Credential(agent_id(AGENT));
    assert_eq!(
        lease_changes(&state),
        vec![
            (1, Some(agent.clone())),
            (2, Some(PrincipalId::Owner)),
            (3, None),
            (4, Some(agent)),
            (5, None),
            (6, Some(PrincipalId::Credential(agent_id(OTHER_AGENT)))),
            (7, None),
        ]
    );
    assert_eq!(state.input_lease_epoch(pane).get(), 7);
    let expiry = state
        .journal
        .retained()
        .last()
        .expect("the expiry was journaled");
    assert!(
        expiry.stamp.actor.is_none(),
        "an expiry is the server's change"
    );
}

/// A detach clears the holder's local and satellite leases in one call; a
/// stranded entry in either ledger leaves a pane nobody can type into.
#[test]
fn detach_releases_local_and_satellite_leases_together() {
    let (mut state, pane) = world();
    let host = phux_protocol::ids::SatelliteHost::from("sat");
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    state.set_input_lease(pane, AGENT);
    state
        .federation_mut()
        .set_satellite_lease(host.clone(), 9, AGENT, tx);

    state.detach(AGENT);

    assert_eq!(state.input_lease_holder(pane), None);
    assert_eq!(state.federation().satellite_lease_holder(&host, 9), None);
}
