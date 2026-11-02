use phux_core::process::ExitOutcome;
use phux_core::resource::AgentFacet;
use phux_protocol::wire::frame::CloseReason;

use super::reap_exited_pane;
use crate::state::ServerState;

fn agent(provider: &str) -> AgentFacet {
    AgentFacet {
        provider: provider.to_owned(),
        native_id: None,
        state: None,
    }
}

#[test]
fn reap_cascades_through_grandchildren() {
    let mut state = ServerState::new();
    let (_session, grandparent) = state.seed_session("main");
    let child = state
        .new_agent_session(grandparent, &agent("child"))
        .expect("child");
    let grandchild = state
        .new_agent_session(grandparent, &agent("grandchild"))
        .expect("grandchild seed");
    state
        .registry_mut()
        .resource_mut(grandchild)
        .expect("live")
        .parent = Some(child);

    let wire_grandparent = state.intern_resource_wire(grandparent);
    let wire_grandchild = state.intern_resource_wire(grandchild);
    let reap = reap_exited_pane(&mut state, grandparent, ExitOutcome::exited(0), None)
        .expect("grandparent was live");

    assert_eq!(reap.wire_terminal_id, wire_grandparent);
    assert!(
        reap.cascaded.iter().any(|c| {
            c.wire_terminal_id == wire_grandchild && c.reason == CloseReason::ParentClosed
        }),
        "grandchild must be in the cascade with ParentClosed, not silently \
             dropped when the child is reaped; got {:?}",
        reap.cascaded
            .iter()
            .map(|c| (&c.wire_terminal_id, c.reason))
            .collect::<Vec<_>>(),
    );
    assert!(
        state.registry().resource(grandchild).is_none(),
        "grandchild must leave the registry with its ancestor"
    );
    // Immediate children still cascade; this is the one-level case the
    // previous helper covered, plus the grandchild.
    assert_eq!(reap.cascaded.len(), 2);
}
