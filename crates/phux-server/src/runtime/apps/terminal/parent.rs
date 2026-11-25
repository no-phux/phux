//! `phux.parent/v1`: the pane a pane was started from. Clients read and
//! write it as ordinary metadata; the server also keeps it on the pane's
//! launch record and reinstalls it at every activation and after an upgrade,
//! as it does the notify mode. It binds no lifecycle: closing the parent
//! leaves the child naming it, and lists show such a child at the top level.

use phux_core::{LaunchSpec, TerminalLaunch};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{RESOURCE_PARENT_KEY, Scope, decode_pane_parent};

use crate::state::{ServerState, SharedState};

/// Why a write of `value` at `(scope, key)` is refused: only a Terminal
/// scope, naming another local pane by its bare id.
pub(crate) fn refusal(scope: &Scope, key: &str, value: &[u8]) -> Option<&'static str> {
    if key != RESOURCE_PARENT_KEY {
        return None;
    }
    let Scope::Resource(pane) = scope else {
        return Some("want a Terminal scope");
    };
    match decode_pane_parent(value) {
        None => Some("want the parent's pane id as bare digits"),
        Some(parent) if pane.local_id() == Some(parent) => Some("a pane cannot be its own parent"),
        Some(_) => None,
    }
}

/// Keep a stored or deleted parent on the pane's launch record. Any other
/// key is ignored.
pub(crate) fn persist(state: &SharedState, scope: &Scope, key: &str, value: Option<&[u8]>) {
    let (RESOURCE_PARENT_KEY, Scope::Resource(terminal)) = (key, scope) else {
        return;
    };
    let parent = value.and_then(decode_pane_parent);
    let result = state
        .with_mut(|s| super::resume::update_launch(s, terminal, |launch| launch.parent = parent));
    if let Err(refusal) = result {
        tracing::debug!(?terminal, %refusal, "pane parent not recorded");
    }
}

/// Install `launch`'s stored parent on the pane `wire`.
pub(crate) fn install(s: &mut ServerState, wire: &ResourceId, launch: &TerminalLaunch) {
    if let Some(parent) = launch.parent {
        s.metadata_set(
            &Scope::Resource(wire.clone()),
            RESOURCE_PARENT_KEY,
            parent.to_string().into_bytes(),
        );
    }
}

/// Install the stored parent of the live pane `wire`, from its record.
pub(crate) fn reinstall(s: &mut ServerState, wire: &ResourceId) {
    let launch = s
        .idspace
        .resource_from_wire(wire)
        .and_then(|id| s.registry().record(id))
        .and_then(|record| match &record.launch {
            Some(LaunchSpec::Terminal(launch)) => Some(launch.clone()),
            _ => None,
        });
    if let Some(launch) = launch {
        install(s, wire, &launch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_outlives_the_metadata_on_the_record_and_a_delete_clears_it() {
        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("kids"));
        let wire = state.with_mut(|s| s.idspace.intern_resource(pane));
        let own = wire.local_id().expect("local pane").to_string();
        let scope = Scope::Resource(wire.clone());
        assert_eq!(refusal(&scope, RESOURCE_PARENT_KEY, b"7"), None);
        for bad in [&b"@7"[..], b"0", b"", b"-1", b" 7"] {
            assert!(
                refusal(&scope, RESOURCE_PARENT_KEY, bad).is_some(),
                "{bad:?}"
            );
        }
        assert!(refusal(&scope, RESOURCE_PARENT_KEY, own.as_bytes()).is_some());
        assert!(refusal(&Scope::Global, RESOURCE_PARENT_KEY, b"7").is_some());

        persist(&state, &scope, RESOURCE_PARENT_KEY, Some(b"7"));
        let stored = || {
            state.with(
                |s| match &s.registry().record(pane).expect("record").launch {
                    Some(LaunchSpec::Terminal(launch)) => launch.parent,
                    other => panic!("terminal launch: {other:?}"),
                },
            )
        };
        assert_eq!(stored(), Some(7));
        // A new server image holds no pane metadata until it is reinstalled.
        state.with_mut(|s| s.metadata_delete(&scope, RESOURCE_PARENT_KEY));
        state.with_mut(|s| reinstall(s, &wire));
        let held = state.with(|s| s.metadata().get(&scope, RESOURCE_PARENT_KEY));
        assert_eq!(held.as_deref(), Some(&b"7"[..]));

        persist(&state, &scope, RESOURCE_PARENT_KEY, None);
        assert_eq!(stored(), None, "a delete clears it");
    }
}
