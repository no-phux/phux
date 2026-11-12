//! Every resource a person can put on screen, listed once for the sidebar,
//! the palette, cycling and the window title: terminals, then documents
//! (trees, surfaces and typed documents), each in the server's order.

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
}

/// The topology's terminals and documents. An agent session has no screen
/// of its own, and a kind this build does not know cannot be drawn.
pub fn list(topology: &Topology) -> Vec<Addressable> {
    let mut out: Vec<_> = topology
        .resources
        .iter()
        .filter(|r| r.kind.is_terminal() || r.kind.is_document())
        .map(|r| {
            let session = match r.id.host() {
                Some(host) => host.as_str().to_owned(),
                None => topology
                    .sessions
                    .iter()
                    .find(|s| s.id == r.session_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
            };
            let titled = r.title.clone().filter(|t| !t.trim().is_empty());
            let label = match topology.pane(&r.id) {
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
        let Some(topology) = &self.topology else {
            return Vec::new();
        };
        let org = self.client.as_ref().map(|client| client.organization());
        let mut all = list(topology);
        all.retain_mut(|r| {
            // A popup's terminal is its box's, never a place to go.
            if self.popup.terminal() == Some(&r.id) {
                return false;
            }
            if !r.kind.is_terminal() {
                let Some(document) = self.resources.entries.get(&r.id) else {
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
        let listed: Vec<_> = list(&Topology::from_snapshot(&snapshot))
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
