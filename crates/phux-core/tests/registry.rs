//! Integration tests for [`phux_core::Registry`].

use phux_core::{LayoutNode, Registry, RegistryError, SessionId, TerminalId, WindowId};

#[test]
fn new_session_window_pane_chain_yields_distinct_lookups() {
    let mut reg = Registry::new();
    let s = reg.new_session("main".to_string());
    let w = reg.new_window(s).expect("session exists");
    let p = reg.new_terminal(w).expect("window exists");

    assert!(reg.session(s).is_some());
    assert!(reg.window(w).is_some());
    let pane = reg.terminal(p).expect("pane exists");
    assert_eq!(pane.id, p);
    assert_eq!(pane.window, w);

    // Parent linkage is bidirectional.
    let session = reg.session(s).expect("session exists");
    assert_eq!(session.windows, vec![w]);
    assert_eq!(session.active, Some(w));

    let window = reg.window(w).expect("window exists");
    assert_eq!(window.session, s);
    assert_eq!(window.panes, vec![p]);
    // A single-pane window's layout is a Leaf for that pane.
    assert_eq!(window.layout, Some(LayoutNode::Leaf(p)));
    assert_eq!(window.active, Some(p));
}

#[test]
fn new_window_against_unknown_session_errors() {
    let mut reg = Registry::new();
    let bogus: SessionId = SessionId::default();
    match reg.new_window(bogus) {
        Err(RegistryError::UnknownSession(id)) => assert_eq!(id, bogus),
        other => panic!("expected UnknownSession, got {other:?}"),
    }
}

#[test]
fn new_terminal_against_unknown_window_errors() {
    let mut reg = Registry::new();
    let bogus: WindowId = WindowId::default();
    match reg.new_terminal(bogus) {
        Err(RegistryError::UnknownWindow(id)) => assert_eq!(id, bogus),
        other => panic!("expected UnknownWindow, got {other:?}"),
    }
}

#[test]
fn remove_terminal_invalidates_key_and_unlinks_from_window() {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_string());
    let w = reg.new_window(s).expect("session exists");
    let p1 = reg.new_terminal(w).expect("window exists");
    let p2 = reg.new_terminal(w).expect("window exists");

    let removed = reg.remove_terminal(p1).expect("pane existed");
    assert_eq!(removed.id, p1);
    assert!(reg.terminal(p1).is_none());

    let window = reg.window(w).expect("window exists");
    assert!(!window.panes.contains(&p1));
    assert_eq!(window.panes, vec![p2]);
    // Layout collapsed the split — p2 is now the sole Leaf.
    assert_eq!(window.layout, Some(LayoutNode::Leaf(p2)));
    // Active rolled forward to the remaining pane.
    assert_eq!(window.active, Some(p2));
}

#[test]
fn remove_terminal_clears_active_when_last() {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_string());
    let w = reg.new_window(s).expect("session exists");
    let p = reg.new_terminal(w).expect("window exists");

    reg.remove_terminal(p).expect("pane existed");
    let window = reg.window(w).expect("window exists");
    assert_eq!(window.active, None);
    assert!(window.panes.is_empty());
    // Layout is cleared when the last pane is removed.
    assert!(window.layout.is_none());
}

#[test]
fn remove_terminal_unknown_returns_none() {
    let mut reg = Registry::new();
    let bogus: TerminalId = TerminalId::default();
    assert!(reg.remove_terminal(bogus).is_none());
}

#[test]
fn remove_window_cascades_to_panes() {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_string());
    let w = reg.new_window(s).expect("session exists");
    let p1 = reg.new_terminal(w).expect("window exists");
    let p2 = reg.new_terminal(w).expect("window exists");

    let removed = reg.remove_window(w).expect("window existed");
    assert_eq!(removed.id, w);
    assert!(reg.window(w).is_none());
    assert!(reg.terminal(p1).is_none());
    assert!(reg.terminal(p2).is_none());

    let session = reg.session(s).expect("session exists");
    assert!(!session.windows.contains(&w));
    assert_eq!(session.active, None);
}

#[test]
fn remove_window_active_rolls_forward() {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_string());
    let w1 = reg.new_window(s).expect("session exists");
    let w2 = reg.new_window(s).expect("session exists");

    reg.remove_window(w1).expect("window existed");
    let session = reg.session(s).expect("session exists");
    assert_eq!(session.active, Some(w2));
    assert_eq!(session.windows, vec![w2]);
}

#[test]
fn remove_session_cascades_fully() {
    let mut reg = Registry::new();
    let s = reg.new_session("s".to_string());
    let w1 = reg.new_window(s).expect("session exists");
    let w2 = reg.new_window(s).expect("session exists");
    let p1 = reg.new_terminal(w1).expect("window exists");
    let p2 = reg.new_terminal(w2).expect("window exists");
    let p3 = reg.new_terminal(w2).expect("window exists");

    let removed = reg.remove_session(s).expect("session existed");
    assert_eq!(removed.id, s);
    assert!(reg.session(s).is_none());
    assert!(reg.window(w1).is_none());
    assert!(reg.window(w2).is_none());
    assert!(reg.terminal(p1).is_none());
    assert!(reg.terminal(p2).is_none());
    assert!(reg.terminal(p3).is_none());
    assert_eq!(reg.session_count(), 0);
    assert_eq!(reg.window_count(), 0);
}

#[test]
fn ids_are_distinct_across_kinds() {
    // Compile-time: SessionId/WindowId/TerminalId cannot be mixed up. This test
    // exists to anchor the property in the suite — the assertion is trivial,
    // the value is the *types* of the locals.
    let mut reg = Registry::new();
    let s: SessionId = reg.new_session("s".to_string());
    let w: WindowId = reg.new_window(s).expect("session exists");
    let p: TerminalId = reg.new_terminal(w).expect("window exists");
    assert_eq!(reg.session_count(), 1);
    assert_eq!(reg.window_count(), 1);
    // Force the IDs to be used so the bindings are not dead code.
    assert_eq!(reg.terminal(p).map(|x| x.id), Some(p));
}

// ---- proptest: random op sequences keep the registry self-consistent ------

use proptest::prelude::*;

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

proptest! {
    #[test]
    fn registry_invariants_hold_under_random_ops(ops in proptest::collection::vec(op_strategy(), 0..64)) {
        let mut reg = Registry::new();
        let mut sessions: Vec<SessionId> = Vec::new();
        let mut windows: Vec<WindowId> = Vec::new();
        let mut panes: Vec<TerminalId> = Vec::new();

        for op in ops {
            match op {
                Op::NewSession => {
                    let id = reg.new_session("p".to_string());
                    sessions.push(id);
                }
                Op::NewWindow(i) => {
                    if !sessions.is_empty() {
                        let s = sessions[i % sessions.len()];
                        if let Ok(w) = reg.new_window(s) {
                            windows.push(w);
                        }
                    }
                }
                Op::NewPane(i) => {
                    if !windows.is_empty() {
                        let w = windows[i % windows.len()];
                        if let Ok(p) = reg.new_terminal(w) {
                            panes.push(p);
                        }
                    }
                }
                Op::RemovePane(i) => {
                    if !panes.is_empty() {
                        let p = panes.swap_remove(i % panes.len());
                        let _ = reg.remove_terminal(p);
                    }
                }
                Op::RemoveWindow(i) => {
                    if !windows.is_empty() {
                        let w = windows.swap_remove(i % windows.len());
                        let _ = reg.remove_window(w);
                    }
                }
                Op::RemoveSession(i) => {
                    if !sessions.is_empty() {
                        let s = sessions.swap_remove(i % sessions.len());
                        let _ = reg.remove_session(s);
                    }
                }
            }

            // Invariant: every WindowId in a Session.windows resolves and
            // links back; every TerminalId in a Window.panes resolves and links
            // back; layout.panes mirrors panes; active references are live.
            let session_ids: Vec<SessionId> = sessions.iter().copied().filter(|id| reg.session(*id).is_some()).collect();
            for sid in session_ids {
                let session = reg.session(sid).expect("filtered to live");
                for wid in &session.windows {
                    let window = reg.window(*wid).expect("session points at live window");
                    prop_assert_eq!(window.session, sid);
                    // Layout leaves match the window's pane set.
                    let leaves: Vec<phux_core::TerminalId> = window
                        .layout
                        .as_ref()
                        .map(LayoutNode::leaves)
                        .unwrap_or_default();
                    let pane_set: std::collections::HashSet<_> = window.panes.iter().copied().collect();
                    let leaf_set: std::collections::HashSet<_> = leaves.iter().copied().collect();
                    prop_assert_eq!(pane_set, leaf_set);
                    prop_assert_eq!(leaves.len(), window.panes.len());
                    for pid in &window.panes {
                        let pane = reg.terminal(*pid).expect("window points at live pane");
                        prop_assert_eq!(pane.window, *wid);
                    }
                    if let Some(a) = window.active {
                        prop_assert!(window.panes.contains(&a));
                    }
                }
                if let Some(a) = session.active {
                    prop_assert!(session.windows.contains(&a));
                }
            }

            // Invariant 3: no orphan panes — every live pane's window is live
            // and lists it.
            // We can't iterate panes directly without exposing slotmap;
            // instead, iterate our tracked panes vec and check those still
            // present in the registry.
            for pid in &panes {
                if let Some(pane) = reg.terminal(*pid) {
                    let window = reg.window(pane.window).expect("pane's parent window must be live");
                    prop_assert!(window.panes.contains(pid));
                }
            }
        }
    }
}

// ---- Registry::sessions() iterator ----------------------------------------

#[test]
fn sessions_iter_is_empty_on_fresh_registry() {
    let reg = Registry::new();
    assert_eq!(reg.sessions().count(), 0);
}

#[test]
fn sessions_iter_yields_every_inserted_session() {
    let mut reg = Registry::new();
    let a = reg.new_session("alpha".to_string());
    let b = reg.new_session("bravo".to_string());
    let c = reg.new_session("charlie".to_string());

    let collected: Vec<(SessionId, String)> =
        reg.sessions().map(|(id, s)| (id, s.name.clone())).collect();
    assert_eq!(collected.len(), 3);
    let names: std::collections::HashSet<&str> =
        collected.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains("alpha"));
    assert!(names.contains("bravo"));
    assert!(names.contains("charlie"));

    // Every yielded id must round-trip through `session()`.
    for (id, _name) in &collected {
        assert!(reg.session(*id).is_some());
    }

    // The three inserted ids must be the ones yielded (set equality).
    let yielded_ids: std::collections::HashSet<SessionId> =
        collected.iter().map(|(id, _)| *id).collect();
    let expected_ids: std::collections::HashSet<SessionId> = [a, b, c].into_iter().collect();
    assert_eq!(yielded_ids, expected_ids);
}

#[test]
fn sessions_iter_drops_removed_sessions() {
    let mut reg = Registry::new();
    let a = reg.new_session("alpha".to_string());
    let b = reg.new_session("bravo".to_string());
    let _ = reg.remove_session(a).expect("alpha was just inserted");

    let collected: Vec<SessionId> = reg.sessions().map(|(id, _)| id).collect();
    assert_eq!(collected, vec![b]);
}

#[test]
fn sessions_iter_supports_find_by_name() {
    // The canonical use case the server crate has been working around.
    let mut reg = Registry::new();
    let _ = reg.new_session("alpha".to_string());
    let target = reg.new_session("bravo".to_string());
    let _ = reg.new_session("charlie".to_string());

    let found = reg
        .sessions()
        .find(|(_, s)| s.name == "bravo")
        .map(|(id, _)| id);
    assert_eq!(found, Some(target));

    let missing = reg.sessions().find(|(_, s)| s.name == "ghost");
    assert!(missing.is_none());
}
