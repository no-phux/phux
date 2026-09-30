//! Filtering at the source (`docs/spec/workload-auth.md` §6).
//!
//! The rows whose result is assembled from several resources admit a scoped
//! grant that holds the row's verb anywhere, then return only what the grant
//! covers.
//!
//! - `ATTACH`: the `ATTACHED` snapshot keeps the Terminals, windows, and
//!   sessions the grant may `OBSERVE`.
//! - `GET_STATE { SERVER }`: the snapshot keeps what the grant may
//!   `INVENTORY`; the listener report, server-global data, needs Global.
//! - `SUBSCRIBE_EVENTS { terminal: None }`: each journaled event is delivered
//!   only when the grant may `OBSERVE` its Terminal (or the Terminal's
//!   parent); an event about no Terminal is server-global and needs Global.
//!
//! The owner's grant sees everything, so a `local` or transitional server, and
//! the owner socket under `paired`, filter nothing.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::kinds::Verb;
use phux_protocol::scope::EffectiveScopeSet;
use phux_protocol::wire::info::SessionSnapshot;

use super::enforce::{Located, Point, TerminalPoint, contains, group_of};
use super::{Authority, ConnectionGrant};
use crate::state::{ClientId, ServerState};

/// What one connection's grant lets it see.
#[derive(Debug, Clone, Copy)]
enum View<'a> {
    /// The owner's grant: nothing is filtered.
    All,
    /// A scoped grant's clauses.
    Scoped(&'a EffectiveScopeSet),
    /// A withdrawn grant: nothing is visible.
    Nothing,
}

impl View<'_> {
    const fn of(grant: Option<&ConnectionGrant>) -> View<'_> {
        // No grant: the dispatch guard refuses every frame from such a
        // connection (`Denial::UNGRANTED`), so only a server-internal caller
        // reaches a result without one, and it filters nothing.
        let Some(grant) = grant else {
            return View::All;
        };
        if grant.revocation().is_some() {
            return View::Nothing;
        }
        match &grant.authority {
            Authority::Owner => View::All,
            Authority::Scoped { effective, .. } => View::Scoped(effective),
        }
    }

    fn admits(self, verb: Verb, point: &Point) -> bool {
        match self {
            Self::All => true,
            Self::Scoped(effective) => covers(effective, verb, point),
            Self::Nothing => false,
        }
    }
}

fn covers(effective: &EffectiveScopeSet, verb: Verb, point: &Point) -> bool {
    effective.admits(verb, |selector| contains(selector, point))
}

/// The clauses a subscription must be filtered by: `None` for the owner's
/// grant, which sees every event.
#[must_use]
pub fn event_filter(grant: Option<&ConnectionGrant>) -> Option<EffectiveScopeSet> {
    match View::of(grant) {
        View::All => None,
        View::Scoped(effective) => Some(effective.clone()),
        View::Nothing => Some(EffectiveScopeSet::default()),
    }
}

/// Where a journaled event sits in the topology, stamped when it is recorded
/// so delivery and replay judge it without the state lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSubject {
    /// The wire session id holding the event's Terminal (or its parent) when
    /// the event was recorded; `None` when it was in no window.
    pub group: Option<u32>,
}

impl EventSubject {
    /// An event not yet placed: in no Group.
    pub const UNPLACED: Self = Self { group: None };

    /// Stamp the event about `terminal` (widened to `parent`) under `s`.
    #[must_use]
    pub fn at(
        s: &ServerState,
        terminal: Option<&WireResourceId>,
        parent: Option<&WireResourceId>,
    ) -> Self {
        let group = [terminal, parent]
            .into_iter()
            .flatten()
            .filter(|wire| wire.is_local())
            .find_map(|wire| {
                s.terminal_from_wire(wire)
                    .and_then(|core| group_of(s, core))
            });
        Self { group }
    }
}

/// Whether a subscription filtered by `effective` may receive an event about
/// `terminal` (widened to `parent`): `OBSERVE` on the Terminal, or on its
/// parent, or on Global for a server-global event.
#[must_use]
pub fn observes_event(
    effective: &EffectiveScopeSet,
    terminal: Option<&WireResourceId>,
    parent: Option<&WireResourceId>,
    subject: &EventSubject,
) -> bool {
    let Some(terminal) = terminal else {
        return covers(effective, Verb::Observe, &Point::Global);
    };
    let point = |wire: &WireResourceId, parent: Option<Box<TerminalPoint>>| TerminalPoint {
        at: located(wire),
        group: wire.is_local().then_some(subject.group).flatten(),
        parent,
    };
    let parent = parent.map(|wire| Box::new(point(wire, None)));
    covers(
        effective,
        Verb::Observe,
        &Point::Terminal(point(terminal, parent)),
    )
}

fn located(wire: &WireResourceId) -> Located {
    match wire {
        WireResourceId::Local { id } => Located::Local(*id),
        WireResourceId::Satellite { host, id } => Located::Satellite(host.as_str().to_owned(), *id),
    }
}

/// `snapshot` as `client` may see it with `verb`.
///
/// `s` is the state the snapshot was cut from. Terminals are judged under
/// the live topology, sessions and windows as their Group, satellite
/// inventory rows as their Host, and the listener report as server-global
/// data.
#[must_use]
pub fn filter_snapshot(
    s: &ServerState,
    client: ClientId,
    verb: Verb,
    mut snapshot: SessionSnapshot,
) -> SessionSnapshot {
    let view = View::of(s.connection_grant(client));
    if matches!(view, View::All) {
        return snapshot;
    }
    let group = |id: u32| view.admits(verb, &Point::Group(id));
    snapshot
        .resources
        .retain(|resource| view.admits(verb, &Point::Terminal(TerminalPoint::of(s, &resource.id))));
    snapshot.sessions.retain(|session| group(session.id.get()));
    snapshot
        .windows
        .retain(|window| group(window.session_id.get()));
    if !snapshot
        .sessions
        .iter()
        .any(|session| session.id == snapshot.focused_session)
    {
        snapshot.focused_session = phux_protocol::ids::SessionId::new(0);
        snapshot.focused_window = phux_protocol::ids::WindowId::new(0);
        snapshot.focused_resource = WireResourceId::local(0);
    }
    let hosts: Vec<_> = snapshot
        .hosts()
        .iter()
        .filter(|row| view.admits(verb, &Point::SatelliteHost(row.host.as_str().to_owned())))
        .cloned()
        .collect();
    snapshot = snapshot.with_hosts(hosts);
    if !view.admits(verb, &Point::Global) {
        snapshot = snapshot.without_listeners();
    }
    snapshot
}
