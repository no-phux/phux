//! Every resource a person can put on screen, listed once for the sidebar,
//! the palette, cycling and the window title: terminals, then documents
//! (trees, surfaces and typed documents), each in the server's order.

use std::collections::HashMap;

use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, ResourceKind};

use crate::desk::Desk;
use crate::palette::pane_label;

/// One resource that can occupy the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressable {
    pub id: ResourceId,
    pub label: String,
    /// Its live session; 0 for a hub's satellite resource.
    pub session_id: u32,
    /// The session's name, or a satellite resource's host.
    pub session: String,
    pub kind: ResourceKind,
    /// What a list calls the document when `!kind.is_terminal()`.
    pub mirror: &'static str,
    /// The pane a terminal was started from, which the sidebar folds it
    /// under (`phux.parent/v1`).
    pub parent: Option<ResourceId>,
}

/// The topology's terminals and documents that `wanted` keeps. An agent
/// session has no screen of its own, and a kind this build does not know
/// cannot be drawn. Linear in the topology: the window lists it on every
/// sidebar build, so a scan of the panes per resource would grow with the
/// square of the server's terminals.
fn list(topology: &Topology, wanted: impl Fn(&ResourceId) -> bool) -> Vec<Addressable> {
    // Reversed so the first entry wins, as a scan would find it.
    let panes: HashMap<_, _> = (topology.panes.iter().rev())
        .map(|pane| (&pane.terminal_id, pane))
        .collect();
    let sessions: HashMap<_, _> = (topology.sessions.iter().rev())
        .map(|session| (session.id, session.name.as_str()))
        .collect();
    let mut out: Vec<_> = topology
        .resources
        .iter()
        .filter(|r| (r.kind.is_terminal() || r.kind.is_document()) && wanted(&r.id))
        .map(|r| {
            let session = match r.id.host() {
                Some(host) => host.as_str().to_owned(),
                None => sessions
                    .get(&r.session_id)
                    .map(|name| (*name).to_owned())
                    .unwrap_or_default(),
            };
            let titled = r.title.clone().filter(|t| !t.trim().is_empty());
            let label = match panes.get(&r.id) {
                Some(pane) => pane_label(pane),
                None => titled.unwrap_or_else(|| kind_name(r.kind).to_owned()),
            };
            Addressable {
                id: r.id.clone(),
                label,
                session_id: r.session_id,
                session,
                kind: r.kind,
                mirror: "Read-only mirror",
                parent: panes.get(&r.id).and_then(|pane| pane.parent.clone()),
            }
        })
        .collect();
    out.sort_by_key(|r| !r.kind.is_terminal());
    out
}

/// What an unnamed document is called.
fn kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::AccessibilityTree => "Accessibility tree",
        ResourceKind::PixelSurface => "Surface",
        ResourceKind::Terminal => "Terminal",
        _ => "Document",
    }
}

impl Desk {
    /// What this window can show: every terminal, and each document it
    /// holds a mirror for, named and placed as its organization record
    /// says. A local resource in no session it can name is left out, since
    /// showing it would attach a session called "".
    pub fn addressable(&self) -> Vec<Addressable> {
        self.addressable_where(|_| true)
    }

    /// [`Self::addressable`]'s entry for `id`, without naming every other
    /// resource: the window title asks on every poll.
    pub fn addressable_of(&self, id: &ResourceId) -> Option<Addressable> {
        self.addressable_where(|r| r == id).into_iter().next()
    }

    fn addressable_where(&self, wanted: impl Fn(&ResourceId) -> bool) -> Vec<Addressable> {
        let Some(topology) = &self.connection.topology else {
            return Vec::new();
        };
        let org = self
            .connection
            .client
            .as_ref()
            .map(|client| client.organization());
        let mut all = list(topology, wanted);
        all.retain_mut(|r| {
            // A popup's terminal is its box's, never a place to go; the quick
            // terminal's is the panel's.
            if self.connection.popup.terminal() == Some(&r.id)
                || (self.quick.is_none() && crate::desk::quick::hidden(&r.session))
            {
                return false;
            }
            if !r.kind.is_terminal() {
                let Some(document) = self.connection.resources.entries.get(&r.id) else {
                    return false;
                };
                let record = &document.record;
                if let Some(name) = record.name.as_ref().filter(|n| !n.trim().is_empty()) {
                    r.label.clone_from(name);
                }
                if r.session.is_empty()
                    && let Some(session) = org.as_ref().and_then(|o| o.session(&record.session))
                {
                    r.session.clone_from(&session.name);
                }
                r.mirror = self.mirror_label(&r.id);
            }
            !r.session.is_empty()
        });
        all
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

    use super::*;

    #[test]
    fn terminals_then_documents_each_named_and_placed() {
        let (work, docs) = (SessionId::new(1), SessionId::new(2));
        let id = ResourceId::local;
        let resource = |n, kind, session| {
            ResourceInfo::new(id(n), WindowId::new(0), 0, 0)
                .with_kind(kind)
                .with_session(session)
        };
        let snapshot = SessionSnapshot::new(work, WindowId::new(1), id(1))
            .with_sessions(vec![
                SessionInfo::new(work, "work"),
                SessionInfo::new(docs, "docs"),
            ])
            .with_resources(vec![
                resource(5, ResourceKind::Document, Some(docs)).with_title(Some("notes".into())),
                ResourceInfo::new(id(1), WindowId::new(1), 80, 24)
                    .with_session(Some(work))
                    .with_title(Some("vim".into())),
                resource(4, ResourceKind::AgentSession, Some(work)),
                resource(3, ResourceKind::PixelSurface, Some(work)),
                resource(2, ResourceKind::AccessibilityTree, Some(docs)),
                resource(9, ResourceKind::Unknown { tag: 90 }, Some(work)),
                ResourceInfo::new(ResourceId::satellite("box", 7), WindowId::new(0), 0, 0)
                    .with_kind(ResourceKind::PixelSurface)
                    .with_title(Some("display".into())),
            ]);
        let mut desk = crate::test_support::window();
        desk.connection.topology = Some(Topology::from_snapshot(&snapshot));
        let all = desk.addressable();
        for id in [id(1), id(5), id(2), id(4), id(42)] {
            let found = all.iter().find(|r| r.id == id).cloned();
            assert_eq!(desk.addressable_of(&id), found, "{id}");
        }
        let listed: Vec<_> = list(&Topology::from_snapshot(&snapshot), |_| true)
            .into_iter()
            .map(|r| (r.id.to_string(), r.label, r.session, r.kind))
            .collect();
        let row = |id: &str, label: &str, session: &str, kind| {
            (id.to_owned(), label.to_owned(), session.to_owned(), kind)
        };
        assert_eq!(
            listed,
            [
                row("@1", "vim", "work", ResourceKind::Terminal),
                row("@5", "notes", "docs", ResourceKind::Document),
                row("@3", "Surface", "work", ResourceKind::PixelSurface),
                row(
                    "@2",
                    "Accessibility tree",
                    "docs",
                    ResourceKind::AccessibilityTree
                ),
                row("box/@7", "display", "box", ResourceKind::PixelSurface),
            ]
        );
    }
}
