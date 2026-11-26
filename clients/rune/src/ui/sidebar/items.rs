//! What a session's rows are: its tabs, then the resources no tab holds.

use phux_protocol::ids::{ResourceId, SessionKey};

use crate::addressable::Addressable;
use crate::desk::{Desk, Tab};

/// One sidebar row's worth of work: a tab and the panes it holds, or a
/// resource no tab holds, alone.
pub(super) struct Item<'a> {
    pub(super) tab: Option<&'a Tab>,
    pub(super) members: Vec<&'a Addressable>,
}

impl<'a> Item<'a> {
    /// The member the row is named after: the first an agent runs in, else
    /// the first. Never the loudest, so a row's name holds still.
    pub(super) fn lead(&self, agent: impl Fn(&ResourceId) -> bool) -> &'a Addressable {
        (self.members.iter().find(|m| agent(&m.id)))
            .or(self.members.first())
            .copied()
            .expect("an item holds a member")
    }
}

/// A session's rows: its tabs, in their order, each holding the listed
/// resources it places; then each listed resource no tab holds, as the
/// server lists them. Without an arrangement, every resource is a row.
pub(super) fn items<'a>(tabs: &'a [Tab], rows: &[&'a Addressable]) -> Vec<Item<'a>> {
    let mut placed = std::collections::HashSet::new();
    let mut out: Vec<Item> = tabs
        .iter()
        .filter_map(|tab| {
            let members: Vec<&Addressable> = (tab.resources.iter())
                .filter_map(|id| rows.iter().find(|r| r.id == *id).copied())
                .filter(|r| placed.insert(r.id.clone()))
                .collect();
            (!members.is_empty()).then_some(Item {
                tab: Some(tab),
                members,
            })
        })
        .collect();
    out.extend(
        (rows.iter())
            .filter(|r| !placed.contains(&r.id))
            .map(|r| Item {
                tab: None,
                members: vec![*r],
            }),
    );
    out
}

/// The row actually drawn for a resource, and its owning local session.
/// A row's lead is its identity, not the pane its press would activate.
pub(crate) fn focus_target(
    desk: &Desk,
    resource: &ResourceId,
) -> Option<(ResourceId, Option<SessionKey>)> {
    // Federated sessions still draw one row per resource, without local folding.
    if resource.host().is_some() {
        return Some((resource.clone(), None));
    }
    let all = desk.addressable();
    let shown = all.iter().find(|r| &r.id == resource)?;
    let session = desk
        .connection
        .topology
        .as_ref()?
        .sessions
        .iter()
        .find(|s| s.id == shown.session_id)?;
    let rows: Vec<_> = all.iter().filter(|r| r.session_id == session.id).collect();
    let tabs = desk.connection.tabs.by_session.get(&session.id);
    let item = items(tabs.map_or(&[][..], Vec::as_slice), &rows)
        .into_iter()
        .find(|item| item.members.iter().any(|m| &m.id == resource))?;
    let lead = item.lead(|id| desk.activities.agent_name(id).is_some());
    Some((lead.id.clone(), session.key))
}

/// A tab's name when a person gave it one: the server's defaults are
/// just numbers.
pub(super) fn given_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.parse::<u32>().is_err()).then(|| name.to_owned())
}
