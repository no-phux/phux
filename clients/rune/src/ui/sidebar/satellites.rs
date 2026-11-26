//! A hub's satellite sessions: each other machine's session under a
//! `host · session` header, its terminals and mirrors as rows.

use phux_client_runtime::control::Topology;
use phux_protocol::ids::{ResourceId, ResourceKind};
use rune::{El, memo};

use super::{Counts, Desk, Forced, Header, Row, Tokens, pane_row, session_header, source_list};
use crate::host_color;
use crate::palette::pane_label;

/// A positively known Terminal. Unknown satellite members never become
/// unchecked terminal targets merely because they are not documents.
fn is_terminal_member(topology: &Topology, id: &ResourceId) -> bool {
    topology.pane(id).is_some()
}

/// A hub's satellite sessions, each under a `host · session` header: its
/// terminals, then the mirrors their producers run (#2051 keeps them out of
/// the hub's own sessions).
pub(super) fn satellite_list(desk: &Desk, topology: &Topology, t: &Tokens) -> El {
    let mut list = source_list(t)
        .key("sidebar-satellites")
        .label("Other machines' sessions");
    let shown = desk.connection.shown.as_ref().map(|s| &s.resource);
    let version = t.version;
    for (n, session) in topology.satellite_sessions.iter().enumerate() {
        let name = format!("{} · {}", session.host, session.name);
        let host = (session.host.as_str(), host_color::entry(&session.host).0);
        let header = Header {
            id: u32::MAX - u32::try_from(n).unwrap_or(0),
            name: &name,
            key: None,
            current: session.members.iter().any(|id| shown == Some(id)),
            counts: Counts::default(),
            count: session.members.len(),
            host: Some(host),
            color: crate::session_color::of(&session.name),
            branch: false,
            target: false,
        };
        list = list.child(memo(
            format!("satellite-{name}"),
            (&header, version),
            || session_header(&header, t),
        ));
        // Members are every kind the session names; documents are listed
        // once below as mirrors and agent sessions have no row of their own.
        let terminals = session
            .members
            .iter()
            .filter(|id| is_terminal_member(topology, id))
            .map(|id| {
                let label = topology.pane(id).map_or_else(|| id.to_string(), pane_label);
                (id, label, ResourceKind::Terminal)
            });
        let mirrors = topology
            .documents
            .iter()
            .filter(|doc| {
                session.members.contains(&doc.id)
                    || doc
                        .parent
                        .as_ref()
                        .is_some_and(|p| session.members.contains(p))
            })
            .map(|doc| {
                let label = doc.name.clone().unwrap_or_else(|| "Mirror".to_owned());
                (&doc.id, label, doc.kind)
            });
        for (id, label, kind) in terminals.chain(mirrors) {
            let resource = crate::addressable::Addressable {
                id: id.clone(),
                label,
                session_id: 0,
                session: session.host.clone(),
                kind,
                mirror: desk.mirror_label(id),
                parent: None,
                program: topology.pane(id).map(crate::addressable::Program::of),
            };
            let presentation = resource.presentation(desk);
            let row = Row {
                id,
                target: id,
                session: "",
                name: presentation.name,
                place: (!presentation.detail.is_empty()).then_some(presentation.detail),
                key: None,
                agent: desk.activities.agent_name(id),
                identity: presentation.identity,
                mark: desk.activities.mark(id),
                panes: 1,
                selected: shown == Some(id),
                members: Vec::new(),
                host,
                color: None,
                state: Forced::Rest,
            };
            list = list.child(memo(format!("pane-{id}"), (&row, version), || {
                pane_row(&row, t)
            }));
        }
    }
    list
}
