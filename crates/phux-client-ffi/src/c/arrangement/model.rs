//! Allocation-bounded catalog/topology projection, independent of replicas.
use std::collections::{HashMap, HashSet};

use phux_client_core::layout::{self, ArrangementDoc, Group, LayoutNode};
use phux_protocol::wire::frame::SessionLifetime;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, ResourceKind, SessionId, WindowId};

use crate::c::client::SessionSummary;
use crate::c::error::BridgeError;

pub(super) const MAX_TEXT: usize = 4096;
pub(super) const MAX_GROUPS: usize = 32;
pub(super) const MAX_NODES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CatalogTerminal {
    pub id: ResourceId,
    pub session: u32,
    pub title: String,
    pub cwd: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Node {
    pub kind: u32,
    pub terminal: Option<ResourceId>,
    pub first: u32,
    pub second: u32,
    pub ratio: f32,
}

#[derive(Default)]
pub(super) struct Catalog {
    pub sessions: Vec<SessionSummary>,
    pub terminals: Vec<CatalogTerminal>,
}

/// The session a snapshot resource belongs to: its `session` facet, or from
/// a server that predates it, the session of its legacy window. A satellite
/// resource without the facet has no session here: satellite window IDs are
/// not keys in this server's registry.
fn session_of(owners: &HashMap<WindowId, u32>, resource: &ResourceInfo) -> u32 {
    if let Some(session) = resource.session {
        return session.get();
    }
    match resource.id {
        ResourceId::Local { .. } => owners.get(&resource.window_id).copied().unwrap_or(0),
        ResourceId::Satellite { .. } => 0,
    }
}

impl Catalog {
    pub(super) fn from_snapshot(
        snapshot: SessionSnapshot,
        selected: u32,
    ) -> Result<Self, BridgeError> {
        if snapshot.sessions.len() > 256
            || snapshot
                .resources
                .iter()
                .filter(|p| p.kind == ResourceKind::Terminal)
                .count()
                > 256
            || snapshot.windows.len() > 256
        {
            return Err(BridgeError::state("catalog capacity exceeded"));
        }
        unique(
            snapshot.windows.iter().map(|w| w.id),
            "duplicate registry window",
        )?;
        let owners: HashMap<_, _> = snapshot
            .windows
            .iter()
            .map(|w| (w.id, w.session_id.get()))
            .collect();
        let mut members: HashMap<u32, u32> = HashMap::new();
        let mut terminals_of: HashMap<u32, u32> = HashMap::new();
        for resource in &snapshot.resources {
            let session = session_of(&owners, resource);
            if session != 0 {
                *members.entry(session).or_default() += 1;
            }
            if resource.kind == ResourceKind::Terminal
                && let Some(session) = resource.session
            {
                *terminals_of.entry(session.get()).or_default() += 1;
            }
        }
        let mut sessions = snapshot
            .sessions
            .into_iter()
            .map(|s| {
                let count = members.get(&s.id.get()).copied().unwrap_or(0);
                let terminals = terminals_of.get(&s.id.get()).copied().unwrap_or(0);
                session_summary(s, selected, count, terminals)
            })
            .collect::<Result<Vec<_>, BridgeError>>()?;
        let mut terminals = snapshot
            .resources
            .into_iter()
            .filter(|p| p.kind == ResourceKind::Terminal)
            .map(|p| catalog_terminal(p, &owners))
            .collect::<Result<Vec<_>, BridgeError>>()?;
        sessions.sort_by_key(|s| s.session_id);
        terminals.sort_by(|a, b| a.id.cmp(&b.id));
        unique(
            sessions.iter().map(|s| s.session_id),
            "duplicate catalog session",
        )?;
        unique(
            terminals.iter().map(|t| &t.id),
            "duplicate catalog terminal",
        )?;
        Ok(Self {
            sessions,
            terminals,
        })
    }

    pub(super) fn allows(&self, id: &ResourceId, selected: u32) -> bool {
        self.terminals.iter().any(|t| {
            &t.id == id && (t.session == selected || matches!(id, ResourceId::Satellite { .. }))
        })
    }

    /// Terminals of `selected` that no group of `doc` shows.
    pub(super) fn unarranged(&self, doc: &ArrangementDoc, selected: u32) -> usize {
        let placed: HashSet<ResourceId> = doc
            .groups
            .iter()
            .filter_map(|group| group.state.tree.as_ref())
            .flat_map(layout::leaves)
            .collect();
        self.terminals
            .iter()
            .filter(|t| t.session == selected && !placed.contains(&t.id))
            .count()
    }
}

/// The frozen `PhuxSessionInfo.window_count`: one for a session with a
/// listed terminal, else zero. A server that reports `SESSION_STATE` (the
/// lifetime) also reports every terminal's `RESOURCE_SESSION` facet, so the
/// count is derived here from `terminals`, the session's facet-joined
/// terminals; only an older server's own legacy count is read.
fn legacy_window_count(session: &SessionInfo, terminals: u32) -> u16 {
    if session.lifetime.is_some() {
        u16::from(terminals > 0)
    } else {
        session.window_count
    }
}

fn session_summary(
    session: SessionInfo,
    selected: u32,
    snapshot_members: u32,
    terminals: u32,
) -> Result<SessionSummary, BridgeError> {
    text(&session.name)?;
    let keep_empty = session.lifetime.map_or(session.keep_empty, |lifetime| {
        lifetime == SessionLifetime::Persistent
    });
    let window_count = legacy_window_count(&session, terminals);
    Ok(SessionSummary {
        session_id: session.id.get(),
        name: session.name.into_bytes(),
        created_at_unix_secs: session.created_at_unix_secs,
        window_count,
        attached_client_count: session.attached_client_count,
        focused: session.id.get() == selected,
        keep_empty,
        key: session.key,
        key_text: session
            .key
            .map(|key| key.to_string().into_bytes())
            .unwrap_or_default(),
        lifetime: session.lifetime,
        snapshot_members,
        org: crate::c::organization::SessionOrg::default(),
    })
}

fn catalog_terminal(
    pane: ResourceInfo,
    owners: &HashMap<WindowId, u32>,
) -> Result<CatalogTerminal, BridgeError> {
    terminal(&pane.id)?;
    let session = session_of(owners, &pane);
    let title = pane.title.unwrap_or_default();
    let cwd = pane.cwd.unwrap_or_default();
    text(&title)?;
    text(&cwd)?;
    Ok(CatalogTerminal {
        id: pane.id,
        session,
        title,
        cwd,
    })
}

pub(super) fn text(value: &str) -> Result<(), BridgeError> {
    if value.len() > MAX_TEXT || value.contains('\0') {
        return Err(BridgeError::invalid(
            "arrangement text exceeds 4096 bytes or contains NUL",
        ));
    }
    Ok(())
}

fn terminal(id: &ResourceId) -> Result<(), BridgeError> {
    match id {
        ResourceId::Local { id } if *id != 0 => Ok(()),
        ResourceId::Satellite { host, id } if *id != 0 && !host.as_str().is_empty() => {
            text(host.as_str())
        }
        _ => Err(BridgeError::invalid(
            "invalid arrangement terminal identity",
        )),
    }
}

fn unique<T: Eq + std::hash::Hash>(
    items: impl Iterator<Item = T>,
    message: &str,
) -> Result<(), BridgeError> {
    let mut seen = HashSet::new();
    if items.into_iter().any(|id| !seen.insert(id)) {
        return Err(BridgeError::invalid(message));
    }
    Ok(())
}

pub(super) fn flatten(arrangement: &ArrangementDoc) -> Result<(Vec<Node>, Vec<u32>), BridgeError> {
    if arrangement.groups.len() > MAX_GROUPS {
        return Err(BridgeError::state("arrangement exceeds 32 groups"));
    }
    unique(
        arrangement.groups.iter().map(|w| w.id),
        "duplicate arrangement group identity",
    )?;
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    for group in &arrangement.groups {
        if group.id == [0; 16] {
            return Err(BridgeError::invalid("unseeded arrangement group identity"));
        }
        text(&group.name)?;
        let tree = group
            .state
            .tree
            .as_ref()
            .ok_or_else(|| BridgeError::invalid("empty group"))?;
        roots.push(flatten_node(tree, &mut nodes, 0)?);
    }
    unique(
        nodes.iter().filter_map(|n| n.terminal.as_ref()),
        "duplicate layout terminal",
    )?;
    Ok((nodes, roots))
}

fn flatten_node(
    tree: &LayoutNode,
    nodes: &mut Vec<Node>,
    depth: usize,
) -> Result<u32, BridgeError> {
    if nodes.len() >= MAX_NODES || depth > 64 {
        return Err(BridgeError::state(
            "arrangement node/depth capacity exceeded",
        ));
    }
    let index =
        u32::try_from(nodes.len()).map_err(|_| BridgeError::state("node index overflow"))?;
    nodes.push(Node {
        kind: 0,
        terminal: None,
        first: 0,
        second: 0,
        ratio: 0.0,
    });
    nodes[index as usize] = flatten_value(tree, nodes, depth)?;
    Ok(index)
}

fn flatten_value(
    tree: &LayoutNode,
    nodes: &mut Vec<Node>,
    depth: usize,
) -> Result<Node, BridgeError> {
    Ok(match tree {
        LayoutNode::Leaf(id) => {
            terminal(id)?;
            Node {
                kind: 1,
                terminal: Some(id.clone()),
                first: 0,
                second: 0,
                ratio: 0.0,
            }
        }
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            if !ratio.is_finite() || *ratio <= 0.0 || *ratio >= 1.0 {
                return Err(BridgeError::invalid("invalid split ratio"));
            }
            Node {
                kind: direction(*dir),
                terminal: None,
                first: flatten_node(left, nodes, depth + 1)?,
                second: flatten_node(right, nodes, depth + 1)?,
                ratio: *ratio,
            }
        }
    })
}

const fn direction(dir: layout::SplitDir) -> u32 {
    match dir {
        layout::SplitDir::Horizontal => 2,
        layout::SplitDir::Vertical => 3,
    }
}

/// Decode the legacy layout metadata value; an absent value is an empty
/// arrangement (the session is not arranged yet), never a synthesized one.
pub(super) fn decode_legacy(bytes: Option<&[u8]>) -> Result<ArrangementDoc, BridgeError> {
    let Some(bytes) = bytes else {
        return Ok(ArrangementDoc::new());
    };
    if bytes.len() > layout::MAX_LAYOUT_METADATA_BYTES {
        return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
    }
    ArrangementDoc::decode_cbor(bytes).map_err(|e| BridgeError::invalid(e.to_string()))
}

/// Validate a read arrangement, drop leaves the catalog does not hold, and
/// keep this client's focus and active group where they still exist.
pub(super) fn adopt(
    mut arrangement: ArrangementDoc,
    previous: &ArrangementDoc,
    catalog: &Catalog,
    selected: u32,
) -> Result<ArrangementDoc, BridgeError> {
    // Validate the entire input before pruning; capacity must never silently truncate.
    flatten(&arrangement)?;
    for group in &mut arrangement.groups {
        prune_group(group, catalog, selected);
    }
    arrangement.prune_empty_groups();
    preserve_focus(&mut arrangement, previous);
    Ok(arrangement)
}

fn prune_group(group: &mut Group, catalog: &Catalog, selected: u32) {
    let Some(tree) = &group.state.tree else {
        return;
    };
    let retired: Vec<_> = layout::leaves(tree)
        .into_iter()
        .filter(|id| !catalog.allows(id, selected))
        .collect();
    for id in retired {
        if let Some(tree) = &group.state.tree {
            group.state.tree = layout::kill_pane(tree, &id).ok().flatten();
        }
    }
}

pub(super) fn preserve_focus(arrangement: &mut ArrangementDoc, previous: &ArrangementDoc) {
    let active = previous.groups.get(previous.active).map(|g| g.id);
    arrangement.active = arrangement
        .groups
        .iter()
        .position(|g| Some(g.id) == active)
        .unwrap_or(0);
    for group in &mut arrangement.groups {
        let leaves = group
            .state
            .tree
            .as_ref()
            .map(layout::leaves)
            .unwrap_or_default();
        let old_focus = previous
            .groups
            .iter()
            .find(|g| g.id == group.id)
            .and_then(|g| g.state.focus.as_ref());
        group.state.focus = old_focus
            .filter(|id| leaves.contains(id))
            .cloned()
            .or_else(|| leaves.first().cloned());
    }
}

pub(super) fn key(selected: u32) -> String {
    layout::layout_key(SessionId::new(selected))
}
