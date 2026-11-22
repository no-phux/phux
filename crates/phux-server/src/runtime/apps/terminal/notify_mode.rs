//! `phux.notify/v1`: a pane's notify mode. Clients read and write it as
//! ordinary metadata; the server also keeps it on the pane's launch record,
//! which outlives the process, and reinstalls it at every activation and
//! after an upgrade.

use phux_core::{LaunchSpec, TerminalLaunch};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{PaneNotify, RESOURCE_NOTIFY_KEY, Scope};

use crate::state::{ServerState, SharedState};

/// Why a write of `value` at `(scope, key)` is refused: only a Terminal
/// scope and one of the three words.
pub(crate) fn refusal(scope: &Scope, key: &str, value: &[u8]) -> Option<&'static str> {
    (key == RESOURCE_NOTIFY_KEY
        && (!matches!(scope, Scope::Resource(_)) || PaneNotify::decode(value).is_none()))
    .then_some("want a Terminal scope and one of on, unread-only, off")
}

/// Keep a stored or deleted mode on the pane's launch record; `on` and a
/// delete both clear it. Any other key is ignored.
pub(crate) fn persist(state: &SharedState, scope: &Scope, key: &str, value: Option<&[u8]>) {
    let (RESOURCE_NOTIFY_KEY, Scope::Resource(terminal)) = (key, scope) else {
        return;
    };
    let mode = value
        .and_then(PaneNotify::decode)
        .filter(|mode| *mode != PaneNotify::On)
        .map(|mode| mode.as_str().to_owned());
    let result = state
        .with_mut(|s| super::resume::update_launch(s, terminal, |launch| launch.notify = mode));
    if let Err(refusal) = result {
        tracing::debug!(?terminal, %refusal, "notify mode not recorded");
    }
}

/// Install `launch`'s stored mode on the pane `wire`.
pub(crate) fn install(s: &mut ServerState, wire: &ResourceId, launch: &TerminalLaunch) {
    if let Some(mode) = launch
        .notify
        .as_deref()
        .and_then(|mode| PaneNotify::decode(mode.as_bytes()))
    {
        s.metadata_set(
            &Scope::Resource(wire.clone()),
            RESOURCE_NOTIFY_KEY,
            mode.as_str().as_bytes().to_vec(),
        );
    }
}

/// Install the stored mode of the live pane `wire`, from its record.
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

    fn stored(state: &SharedState, pane: phux_core::ids::ResourceId) -> Option<String> {
        state.with(
            |s| match &s.registry().record(pane).expect("record").launch {
                Some(LaunchSpec::Terminal(launch)) => launch.notify.clone(),
                other => panic!("terminal launch: {other:?}"),
            },
        )
    }

    #[test]
    fn a_mode_outlives_the_metadata_on_the_record_and_on_clears_it() {
        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("mute"));
        let wire = state.with_mut(|s| s.idspace.intern_resource(pane));
        let scope = Scope::Resource(wire.clone());
        assert_eq!(refusal(&scope, RESOURCE_NOTIFY_KEY, b"unread-only"), None);
        assert!(refusal(&scope, RESOURCE_NOTIFY_KEY, b"mute").is_some());
        assert!(refusal(&Scope::Global, RESOURCE_NOTIFY_KEY, b"off").is_some());

        persist(&state, &scope, RESOURCE_NOTIFY_KEY, Some(b"off"));
        assert_eq!(stored(&state, pane).as_deref(), Some("off"));
        // A new server image holds no pane metadata until it is reinstalled.
        state.with_mut(|s| s.metadata_delete(&scope, RESOURCE_NOTIFY_KEY));
        state.with_mut(|s| reinstall(s, &wire));
        let held = state.with(|s| s.metadata().get(&scope, RESOURCE_NOTIFY_KEY));
        assert_eq!(held.as_deref(), Some(&b"off"[..]));

        persist(&state, &scope, RESOURCE_NOTIFY_KEY, Some(b"on"));
        assert_eq!(stored(&state, pane), None);
        persist(&state, &scope, RESOURCE_NOTIFY_KEY, Some(b"unread-only"));
        persist(&state, &scope, RESOURCE_NOTIFY_KEY, None);
        assert_eq!(stored(&state, pane), None, "a delete clears it");
    }
}
