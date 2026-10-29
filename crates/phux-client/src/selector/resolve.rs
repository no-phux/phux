use std::collections::HashMap;

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::info::SessionSnapshot;

use crate::layout::ArrangementDoc;
use crate::org::{KeyMatch, OrgInventory};

use super::{GroupRef, Selector};
#[cfg(doc)]
use super::{parse, resolve_agent};

/// Resolve a parsed [`Selector`] to the [`ResourceId`]s it names; empty on
/// a miss.
#[must_use]
pub fn resolve(selector: &Selector, snapshot: &SessionSnapshot) -> Vec<ResourceId> {
    resolve_with_tags(selector, snapshot, &TagIndex::new())
}

/// A map from `ResourceId` to its L3 tags (`phux.tags/v1`).
pub type TagIndex = std::collections::HashMap<ResourceId, Vec<String>>;

/// Session arrangements in handle space, by live session id.
pub type Arrangements = HashMap<SessionId, ArrangementDoc>;

/// What a selector may need beyond the snapshot: tags for `#tag`,
/// arrangements for group forms, organization records for `^key`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveContext<'a> {
    /// Tags for `#tag`.
    pub tags: Option<&'a TagIndex>,
    /// Arrangements for `name:N` and `name:N.M`.
    pub arrangements: Option<&'a Arrangements>,
    /// Organization records for `^key`.
    pub organization: Option<&'a OrgInventory>,
}

/// Like [`resolve`], but a `#tag` yields every Terminal carrying the tag in
/// `tags`, in snapshot order (ADR-0027).
///
/// [`Selector::Agent`] yields nothing, on purpose: callers follow this with
/// [`pick_target_pane`], which must never narrow `%name` to an arbitrary pane
/// (ADR-0075 point 3). Branch to [`resolve_agent`] first.
#[must_use]
pub fn resolve_with_tags(
    selector: &Selector,
    snapshot: &SessionSnapshot,
    tags: &TagIndex,
) -> Vec<ResourceId> {
    resolve_in(
        selector,
        snapshot,
        &ResolveContext {
            tags: Some(tags),
            ..ResolveContext::default()
        },
    )
}

/// Resolve `selector` with everything `context` supplies.
///
/// `#tag` resolves against its tags, group forms against its arrangements,
/// `^key` against its organization records. A form whose input is missing
/// falls back (group forms to legacy windows) or misses (`#tag`, `^key`).
#[must_use]
pub fn resolve_in(
    selector: &Selector,
    snapshot: &SessionSnapshot,
    context: &ResolveContext<'_>,
) -> Vec<ResourceId> {
    match selector {
        Selector::Current => terminals_in_session(snapshot, snapshot.focused_session),
        Selector::Session(name) => session_id_by_name(snapshot, name)
            .map_or_else(Vec::new, |sid| terminals_in_session(snapshot, sid)),
        Selector::Group(name, group) => resolve_group(snapshot, context, name, group),
        Selector::Pane(name, group, pane_index) => {
            let panes = resolve_group(snapshot, context, name, group);
            panes
                .into_iter()
                .nth(*pane_index as usize)
                .into_iter()
                .collect()
        }
        Selector::ResourceId(id) => resolve_wire_id(snapshot, ResourceId::local(*id)),
        Selector::SatelliteResourceId { host, id } => {
            resolve_wire_id(snapshot, ResourceId::satellite(host.as_str(), *id))
        }
        Selector::Tag(tag) => {
            let Some(tags) = context.tags else {
                return Vec::new();
            };
            crate::resource::terminals(snapshot)
                .map(|p| p.id.clone())
                .filter(|id| tags.get(id).is_some_and(|ts| ts.iter().any(|t| t == tag)))
                .collect()
        }
        Selector::Agent(_) => Vec::new(),
        Selector::Key(prefix) => resolve_key(snapshot, context.organization, prefix),
    }
}

/// The live Terminals a key prefix names: a session's, or one resource's
/// when it executes here.
fn resolve_key(
    snapshot: &SessionSnapshot,
    organization: Option<&OrgInventory>,
    prefix: &str,
) -> Vec<ResourceId> {
    let Some(organization) = organization else {
        return Vec::new();
    };
    match organization.resolve_key_prefix(prefix) {
        Ok(KeyMatch::Session(session)) => session
            .live
            .map_or_else(Vec::new, |live| terminals_in_session(snapshot, live)),
        Ok(KeyMatch::Resource(resource)) => resource
            .live
            .filter(|id| crate::resource::find(snapshot, id).is_some())
            .into_iter()
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// The live session a whole-session selector (`.` or `name`) targets, so
/// `phux kill` can tear it down in one round trip; `None` for every
/// narrower form.
#[must_use]
pub fn whole_session_name(selector: &Selector, snapshot: &SessionSnapshot) -> Option<String> {
    let session_id = match selector {
        Selector::Current => snapshot.focused_session,
        Selector::Session(name) => session_id_by_name(snapshot, name)?,
        Selector::Group(..)
        | Selector::Pane(..)
        | Selector::ResourceId(_)
        | Selector::SatelliteResourceId { .. }
        | Selector::Tag(_)
        | Selector::Agent(_)
        | Selector::Key(_) => return None,
    };
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == session_id)
        .map(|s| s.name.clone())
}

/// Render a wire id as the canonical direct selector (`@N` or `host/@N`),
/// which round-trips through [`parse`].
#[must_use]
pub fn format_terminal_id(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{}/@{id}", host.as_str()),
    }
}

/// Choose one pane from a selector's `candidates`: the `focused` one if
/// present, else the first; `None` on a miss.
#[must_use]
pub fn pick_target_pane(candidates: &[ResourceId], focused: &ResourceId) -> Option<ResourceId> {
    candidates
        .iter()
        .find(|id| *id == focused)
        .or_else(|| candidates.first())
        .cloned()
}

fn resolve_wire_id(snapshot: &SessionSnapshot, wanted: ResourceId) -> Vec<ResourceId> {
    snapshot
        .resources
        .iter()
        .any(|pane| pane.id == wanted)
        .then_some(wanted)
        .into_iter()
        .collect()
}

/// All Terminal-kind resources in `session`, in snapshot order.
fn terminals_in_session(snapshot: &SessionSnapshot, session: SessionId) -> Vec<ResourceId> {
    crate::resource::terminals_in(snapshot, session)
        .map(|p| p.id.clone())
        .collect()
}

/// Terminals in the group `name`/`group` names: the group's leaves in
/// depth-first order when the session's arrangement is known; else, from a
/// server that reports resource sessions, group `0` (or `window-0`) is all
/// the session's Terminals; else (an older server) the legacy window's Terminals in
/// snapshot order.
fn resolve_group(
    snapshot: &SessionSnapshot,
    context: &ResolveContext<'_>,
    name: &str,
    group: &GroupRef,
) -> Vec<ResourceId> {
    let Some(sid) = session_id_by_name(snapshot, name) else {
        return Vec::new();
    };
    if let Some(arrangement) = context.arrangements.and_then(|all| all.get(&sid)) {
        let found = match group {
            GroupRef::Index(n) => arrangement.groups.get(usize::from(*n)),
            GroupRef::Tag(tag) => arrangement.groups.iter().find(|g| g.name == *tag),
        };
        return found
            .and_then(|g| g.state.tree.as_ref())
            .map(crate::layout::leaves)
            .unwrap_or_default()
            .into_iter()
            .filter(|id| {
                crate::resource::find(snapshot, id).is_some_and(crate::resource::is_terminal)
            })
            .collect();
    }
    if crate::resource::reports_sessions(snapshot) {
        // A server that reports each resource's session keeps no windows of
        // its own: an unarranged session is one group, named by index 0 or
        // by the name its synthesized window carried.
        return match group {
            GroupRef::Index(0) => terminals_in_session(snapshot, sid),
            GroupRef::Tag(tag) if tag == SYNTHESIZED_GROUP_NAME => {
                terminals_in_session(snapshot, sid)
            }
            _ => Vec::new(),
        };
    }
    let window_id = snapshot
        .windows
        .iter()
        .filter(|w| w.session_id == sid)
        .find(|w| match group {
            GroupRef::Index(n) => w.index == *n,
            GroupRef::Tag(tag) => w.name == *tag,
        })
        .map(|w| w.id);
    window_id.map_or_else(Vec::new, |wid| {
        crate::resource::terminals_in(snapshot, sid)
            .filter(|p| p.window_id == wid)
            .map(|p| p.id.clone())
            .collect()
    })
}

/// The name a server that reports resource sessions gives each session's
/// one synthesized window (ADR-0161 §7), which `phux agent` still prints:
/// it keeps naming an unarranged session's one group.
const SYNTHESIZED_GROUP_NAME: &str = "window-0";

fn session_id_by_name(snapshot: &SessionSnapshot, name: &str) -> Option<SessionId> {
    snapshot
        .sessions
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
}
