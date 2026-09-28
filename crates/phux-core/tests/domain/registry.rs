//! Integration tests for [`phux_core::Registry`].

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;

use phux_core::{
    AgentFacet, LayoutNode, Registry, RegistryError, ResourceId, ResourceKind, SessionId, WindowId,
};
use proptest::prelude::*;

fn agent(provider: &str) -> AgentFacet {
    AgentFacet {
        provider: provider.to_owned(),
        native_id: None,
        state: None,
    }
}

/// A registry with one session and one window.
fn session_window() -> (Registry, SessionId, WindowId) {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_owned());
    let w = reg.new_window(s).expect("session exists");
    (reg, s, w)
}

#[test]
fn creation_links_parents_and_children_both_ways() {
    let (mut reg, s, w) = session_window();
    let p = reg.new_terminal(w).expect("window exists");

    let pane = reg.resource(p).expect("pane exists");
    assert_eq!(pane.kind, ResourceKind::Terminal);
    assert_eq!(pane.window, Some(w));
    let session = reg.session(s).expect("session exists");
    assert_eq!(
        (session.windows.clone(), session.active),
        (vec![w], Some(w))
    );
    let window = reg.window(w).expect("window exists");
    assert_eq!(window.session, s);
    assert_eq!(window.slots, vec![p]);
    assert_eq!(window.layout, Some(LayoutNode::Leaf(p)));
    assert_eq!(window.active, Some(p));

    assert_eq!(
        reg.new_window(SessionId::default()),
        Err(RegistryError::UnknownSession(SessionId::default()))
    );
    assert_eq!(
        reg.new_terminal(WindowId::default()),
        Err(RegistryError::UnknownWindow(WindowId::default()))
    );
    assert!(reg.remove_resource(ResourceId::default()).is_none());
}

#[test]
fn removing_terminals_collapses_layout_and_rolls_focus() {
    let (mut reg, _, w) = session_window();
    let p1 = reg.new_terminal(w).expect("window exists");
    let p2 = reg.new_terminal(w).expect("window exists");

    assert_eq!(reg.remove_resource(p1).map(|r| r.id), Some(p1));
    assert!(reg.terminal(p1).is_none());
    let window = reg.window(w).expect("window exists");
    assert_eq!(window.slots, vec![p2]);
    assert_eq!(window.layout, Some(LayoutNode::Leaf(p2)));
    assert_eq!(window.active, Some(p2));

    reg.remove_resource(p2).expect("pane existed");
    let window = reg.window(w).expect("an emptied window persists");
    assert!(window.slots.is_empty());
    assert_eq!((window.layout.as_ref(), window.active), (None, None));
}

#[test]
fn window_and_session_removal_cascade_through_resources_and_children() {
    let (mut reg, s, w1) = session_window();
    let w2 = reg.new_window(s).expect("session exists");
    let t1 = reg.new_terminal(w1).expect("window exists");
    let t2 = reg.new_terminal(w2).expect("window exists");
    let a1 = reg
        .new_agent_session(t1, agent("claude"))
        .expect("terminal parent");
    let a2 = reg
        .new_agent_session(t2, agent("claude"))
        .expect("terminal parent");

    reg.remove_window(w1).expect("window existed");
    assert!(reg.window(w1).is_none());
    assert!(reg.resource(t1).is_none() && reg.resource(a1).is_none());
    assert!(reg.resource(a2).is_some());
    let session = reg.session(s).expect("session exists");
    assert_eq!(session.windows, vec![w2]);
    assert_eq!(session.active, Some(w2), "active rolls forward");

    reg.remove_session(s).expect("session existed");
    assert!(reg.window(w2).is_none());
    assert_eq!(reg.resources().count(), 0);
    assert_eq!(reg.session_count(), 0);
}

#[test]
fn move_terminal_reparents_across_sessions() {
    // ADR-0056: the id is stable, the source drops the leaf, the destination
    // gains it; an emptied source window persists for the caller to reap.
    let (mut reg, _, w1) = session_window();
    let p1 = reg.new_terminal(w1).expect("window exists");
    let p2 = reg.new_terminal(w1).expect("window exists");
    let s2 = reg.new_session("b".to_owned());
    let w2 = reg.new_window(s2).expect("session exists");
    let w3 = reg.new_window(s2).expect("session exists");
    let p3 = reg.new_terminal(w2).expect("window exists");
    let child = reg
        .new_agent_session(p1, agent("claude"))
        .expect("terminal parent");

    reg.move_terminal(p1, w2).expect("move succeeds");
    assert_eq!(reg.resource(p1).and_then(|r| r.window), Some(w2));
    assert_eq!(reg.resource(child).and_then(|r| r.parent), Some(p1));
    let source = reg.window(w1).expect("window exists");
    assert_eq!(source.slots, vec![p2]);
    assert_eq!(source.layout, Some(LayoutNode::Leaf(p2)));
    assert_eq!(source.active, Some(p2));
    assert_eq!(reg.window(w2).expect("window exists").slots, vec![p3, p1]);

    reg.move_terminal(p2, w3).expect("move succeeds");
    let source = reg.window(w1).expect("window persists");
    assert!(source.slots.is_empty() && source.layout.is_none());
    let dest = reg.window(w3).expect("window exists");
    assert_eq!(
        (dest.layout.clone(), dest.active),
        (Some(LayoutNode::Leaf(p2)), Some(p2))
    );

    let before = reg.window(w2).expect("window exists").clone();
    reg.move_terminal(p1, w2).expect("no-op move succeeds");
    assert_eq!(reg.window(w2).expect("window exists").layout, before.layout);

    assert_eq!(
        reg.move_terminal(p1, WindowId::default()),
        Err(RegistryError::UnknownWindow(WindowId::default()))
    );
    assert_eq!(
        reg.move_terminal(ResourceId::default(), w1),
        Err(RegistryError::UnknownResource(ResourceId::default()))
    );
    assert_eq!(
        reg.move_terminal(child, w1),
        Err(RegistryError::UnknownResource(child)),
        "an agent session holds no slot to move"
    );
    assert_eq!(reg.resource(p1).and_then(|r| r.window), Some(w2));
}

#[test]
fn agent_session_binds_to_a_live_terminal_parent_and_holds_no_slot() {
    let (mut reg, _, w) = session_window();
    let t = reg.new_terminal(w).expect("window exists");
    let a = reg
        .new_agent_session(t, agent("claude"))
        .expect("terminal parent");

    let desc = reg.resource(a).expect("agent session exists");
    assert_eq!(desc.kind, ResourceKind::AgentSession);
    assert_eq!((desc.parent, desc.window), (Some(t), None));
    assert_eq!(desc.agent().map(|f| f.provider.as_str()), Some("claude"));
    assert!(
        reg.terminal(a).is_none(),
        "no Terminal facet on an agent session"
    );
    assert_eq!(reg.window(w).expect("window exists").slots, vec![t]);
    assert_eq!(reg.children(t), vec![a]);

    let bogus = ResourceId::default();
    assert_eq!(
        reg.new_agent_session(bogus, agent("claude")),
        Err(RegistryError::UnknownResource(bogus))
    );
    assert_eq!(
        reg.new_agent_session(a, agent("claude")),
        Err(RegistryError::ParentKindMismatch {
            parent: a,
            actual: ResourceKind::AgentSession,
            required: ResourceKind::Terminal,
        })
    );
    assert_eq!(reg.resources().count(), 2, "nothing inserted on error");
}

#[test]
fn removing_a_parent_removes_descendants_but_not_vice_versa() {
    let (mut reg, _, w) = session_window();
    let t = reg.new_terminal(w).expect("window exists");
    let sibling = reg
        .new_agent_session(t, agent("codex"))
        .expect("terminal parent");
    let child = reg
        .new_agent_session(t, agent("child"))
        .expect("terminal parent");
    let grandchild = reg.new_agent_session(t, agent("grandchild")).expect("seed");
    reg.resource_mut(grandchild).expect("live").parent = Some(child);

    reg.remove_resource(sibling).expect("child existed");
    assert!(
        reg.resource(t).is_some(),
        "closing a child leaves the parent"
    );
    assert_eq!(reg.children(t), vec![child]);

    reg.remove_resource(t).expect("parent existed");
    assert!(
        reg.resource(grandchild).is_none(),
        "closing an ancestor must close its grandchild, not orphan it"
    );
    assert_eq!(reg.resources().count(), 0);
}

// ---- proptest: random op sequences keep the registry self-consistent ------

#[derive(Debug, Clone)]
enum Op {
    NewSession,
    NewWindow(usize),
    NewPane(usize),
    RemovePane(usize),
    RemoveWindow(usize),
    RemoveSession(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::NewSession),
        any::<usize>().prop_map(Op::NewWindow),
        any::<usize>().prop_map(Op::NewPane),
        any::<usize>().prop_map(Op::RemovePane),
        any::<usize>().prop_map(Op::RemoveWindow),
        any::<usize>().prop_map(Op::RemoveSession),
    ]
}

fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
    (!items.is_empty()).then(|| items[i % items.len()])
}

fn take<T>(items: &mut Vec<T>, i: usize) -> Option<T> {
    (!items.is_empty()).then(|| items.swap_remove(i % items.len()))
}

/// Every link resolves both ways, layout leaves equal the slot set, and
/// active references are live.
fn check_invariants(reg: &Registry, panes: &[ResourceId]) -> Result<(), TestCaseError> {
    for (sid, session) in reg.sessions() {
        for wid in &session.windows {
            let window = reg.window(*wid).expect("session points at live window");
            prop_assert_eq!(window.session, sid);
            let leaves = window
                .layout
                .as_ref()
                .map(LayoutNode::leaves)
                .unwrap_or_default();
            prop_assert_eq!(leaves.len(), window.slots.len());
            let leaf_set: HashSet<_> = leaves.into_iter().collect();
            let slot_set: HashSet<_> = window.slots.iter().copied().collect();
            prop_assert_eq!(leaf_set, slot_set);
            for pid in &window.slots {
                let pane = reg.resource(*pid).expect("window points at live pane");
                prop_assert_eq!(pane.window, Some(*wid));
            }
            if let Some(a) = window.active {
                prop_assert!(window.slots.contains(&a));
            }
        }
        if let Some(a) = session.active {
            prop_assert!(session.windows.contains(&a));
        }
    }
    for pid in panes {
        if let Some(pane) = reg.resource(*pid) {
            let wid = pane.window.expect("a Terminal holds a window slot");
            let window = reg.window(wid).expect("pane's window must be live");
            prop_assert!(window.slots.contains(pid));
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn registry_invariants_hold_under_random_ops(ops in proptest::collection::vec(op_strategy(), 0..64)) {
        let mut reg = Registry::new();
        let mut sessions: Vec<SessionId> = Vec::new();
        let mut windows: Vec<WindowId> = Vec::new();
        let mut panes: Vec<ResourceId> = Vec::new();

        for op in ops {
            match op {
                Op::NewSession => sessions.push(reg.new_session("p".to_owned())),
                Op::NewWindow(i) => {
                    if let Some(w) = pick(&sessions, i).and_then(|s| reg.new_window(s).ok()) {
                        windows.push(w);
                    }
                }
                Op::NewPane(i) => {
                    if let Some(p) = pick(&windows, i).and_then(|w| reg.new_terminal(w).ok()) {
                        panes.push(p);
                    }
                }
                Op::RemovePane(i) => {
                    if let Some(p) = take(&mut panes, i) {
                        let _ = reg.remove_resource(p);
                    }
                }
                Op::RemoveWindow(i) => {
                    if let Some(w) = take(&mut windows, i) {
                        let _ = reg.remove_window(w);
                    }
                }
                Op::RemoveSession(i) => {
                    if let Some(s) = take(&mut sessions, i) {
                        let _ = reg.remove_session(s);
                    }
                }
            }
            check_invariants(&reg, &panes)?;
        }
    }
}
