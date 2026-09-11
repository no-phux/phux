//! Allocation-bounded catalog/topology projection, independent of replicas.
use std::collections::{HashMap, HashSet};

use phux_client_core::layout::{self, LayoutNode, LayoutState, WindowState, Workspace};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};
use phux_protocol::{ResourceId, ResourceKind, SessionId, WindowId};

use crate::client::SessionSummary;
use crate::error::BridgeError;

pub(super) const MAX_TEXT: usize = 4096;
pub(super) const MAX_WINDOWS: usize = 32;
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
        let mut sessions = snapshot
            .sessions
            .into_iter()
            .map(|s| session_summary(s, selected))
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

    pub(super) fn fallback(&self, selected: u32) -> Workspace {
        let mut terminals: Vec<_> = self
            .terminals
            .iter()
            .filter(|t| t.session == selected)
            .collect();
        terminals.sort_by_key(|t| t.id.local_id());
        Workspace {
            windows: terminals
                .into_iter()
                .map(|t| {
                    WindowState::new(
                        t.id.local_id().unwrap_or_default().to_string(),
                        LayoutState::single(t.id.clone()),
                    )
                })
                .collect(),
            active: 0,
        }
    }
}

fn session_summary(session: SessionInfo, selected: u32) -> Result<SessionSummary, BridgeError> {
    text(&session.name)?;
    Ok(SessionSummary {
        session_id: session.id.get(),
        name: session.name.into_bytes(),
        created_at_unix_secs: session.created_at_unix_secs,
        window_count: session.window_count,
        attached_client_count: session.attached_client_count,
        focused: session.id.get() == selected,
        keep_empty: session.keep_empty,
    })
}

fn catalog_terminal(
    pane: ResourceInfo,
    owners: &HashMap<WindowId, u32>,
) -> Result<CatalogTerminal, BridgeError> {
    terminal(&pane.id)?;
    let title = pane.title.unwrap_or_default();
    let cwd = pane.cwd.unwrap_or_default();
    text(&title)?;
    text(&cwd)?;
    // Satellite numeric window IDs are not keys in this server's registry.
    let session = match pane.id {
        ResourceId::Local { .. } => owners.get(&pane.window_id).copied().unwrap_or(0),
        ResourceId::Satellite { .. } => 0,
    };
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
            "workspace text exceeds 4096 bytes or contains NUL",
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
        _ => Err(BridgeError::invalid("invalid workspace terminal identity")),
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

pub(super) fn flatten(workspace: &Workspace) -> Result<(Vec<Node>, Vec<u32>), BridgeError> {
    if workspace.windows.len() > MAX_WINDOWS {
        return Err(BridgeError::state("workspace exceeds 32 windows"));
    }
    unique(
        workspace.windows.iter().map(|w| w.id),
        "duplicate layout window identity",
    )?;
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    for window in &workspace.windows {
        if window.id == [0; 16] {
            return Err(BridgeError::invalid("unseeded layout window identity"));
        }
        text(&window.name)?;
        let tree = window
            .state
            .tree
            .as_ref()
            .ok_or_else(|| BridgeError::invalid("empty window"))?;
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
        return Err(BridgeError::state("workspace node/depth capacity exceeded"));
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
                kind: direction(*dir)?,
                terminal: None,
                first: flatten_node(left, nodes, depth + 1)?,
                second: flatten_node(right, nodes, depth + 1)?,
                ratio: *ratio,
            }
        }
        _ => return Err(BridgeError::invalid("unknown layout node")),
    })
}

fn direction(dir: layout::SplitDir) -> Result<u32, BridgeError> {
    match dir {
        layout::SplitDir::Horizontal => Ok(2),
        layout::SplitDir::Vertical => Ok(3),
        _ => Err(BridgeError::invalid("unknown split direction")),
    }
}

pub(super) fn adopt(
    bytes: Option<&[u8]>,
    previous: &Workspace,
    catalog: &Catalog,
    selected: u32,
) -> Result<Workspace, BridgeError> {
    let mut workspace = match bytes {
        Some(bytes) => {
            if bytes.len() > 256 * 1024 {
                return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
            }
            Workspace::decode_cbor(bytes).map_err(|e| BridgeError::invalid(e.to_string()))?
        }
        None => catalog.fallback(selected),
    };
    // Validate the entire input before pruning; capacity must never silently truncate.
    flatten(&workspace)?;
    for window in &mut workspace.windows {
        prune_window(window, catalog, selected);
    }
    workspace.prune_empty_windows();
    preserve_focus(&mut workspace, previous);
    Ok(workspace)
}

fn prune_window(window: &mut WindowState, catalog: &Catalog, selected: u32) {
    let Some(tree) = &window.state.tree else {
        return;
    };
    let retired: Vec<_> = layout::leaves(tree)
        .into_iter()
        .filter(|id| !catalog.allows(id, selected))
        .collect();
    for id in retired {
        if let Some(tree) = &window.state.tree {
            window.state.tree = layout::kill_pane(tree, &id).ok().flatten();
        }
    }
}

pub(super) fn preserve_focus(workspace: &mut Workspace, previous: &Workspace) {
    let active = previous.windows.get(previous.active).map(|w| w.id);
    workspace.active = workspace
        .windows
        .iter()
        .position(|w| Some(w.id) == active)
        .unwrap_or(0);
    for window in &mut workspace.windows {
        let leaves = window
            .state
            .tree
            .as_ref()
            .map(layout::leaves)
            .unwrap_or_default();
        let old_focus = previous
            .windows
            .iter()
            .find(|w| w.id == window.id)
            .and_then(|w| w.state.focus.as_ref());
        window.state.focus = old_focus
            .filter(|id| leaves.contains(id))
            .cloned()
            .or_else(|| leaves.first().cloned());
    }
}

pub(super) fn key(selected: u32) -> String {
    format!("phux.tui.layout/v1/{}", SessionId::new(selected).get())
}
