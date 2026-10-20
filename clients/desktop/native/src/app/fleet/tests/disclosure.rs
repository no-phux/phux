//! Folding sessions: where they start, what opens them, what a fold forgets.

use super::sidebar::with_pane;
use super::*;

#[gpui::test]
fn a_long_list_reads_as_a_list(cx: &mut TestAppContext) {
    use gpui::{Modifiers, VisualTestContext};
    let _dial = DeterministicDial::arm();
    cx.update(|cx| cx.set_reduce_motion(true));
    let (handle, desk, _) = launch(cx);
    let cx = &mut VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        desk.prefs.sidebar_visible = true;
        desk.prefs.sidebar_grouping = crate::settings::SidebarGrouping::None;
        let mut local = with_pane(connection(1, vec![session(7, "argo"), session(9, "idle")]));
        // Two terminals with one label, in the one session that has any.
        let mut twin = local.topology.as_ref().unwrap().panes[0].clone();
        twin.terminal_id = ResourceId::local(2);
        local.topology.as_mut().unwrap().panes.push(twin);
        install(desk, local);
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_pane(connection(2, vec![session(8, "argo")])),
        );
        cx.notify();
    });
    draw(cx);
    // Every session starts folded, on one line.
    assert!(!shown(cx, "pane-@1") && !shown(cx, "catalog-pane-mini-@1"));
    // Empty sessions have nothing to open and sink below the ones with work.
    assert!(!shown(cx, "disclose-work-Local-9"));
    let idle = cx.debug_bounds("work-Local-9").unwrap();
    for busy in ["work-Local-7", "work-mini-8"] {
        assert!(cx.debug_bounds(busy).unwrap().bottom() <= idle.top());
    }
    // A name on two machines says which; a unique one does not.
    assert!(shown(cx, "session-words-Local-7") && shown(cx, "session-words-mini-8"));
    assert!(!shown(cx, "session-words-Local-9"));
    // Option-click opens every session that has rows; a plain click folds one.
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), Modifiers::alt());
    draw(cx);
    assert!(shown(cx, "pane-@1") && shown(cx, "catalog-pane-mini-@1"));
    desk.read_with(cx, |desk, _| {
        assert!(desk.remote.is_none(), "a caret never selects its session");
    });
    // Same-labelled terminals carry their ids.
    assert!(shown(cx, "pane-suffix-@1") && shown(cx, "pane-suffix-@2"));
    desk.read_with(cx, |desk, _| assert_eq!(desk.expanded.len(), 2));
    click_on(cx, "disclose-work-Local-7");
    assert!(!shown(cx, "pane-@1") && shown(cx, "catalog-pane-mini-@1"));
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), Modifiers::alt());
    draw(cx);
    desk.read_with(cx, |desk, _| assert!(desk.expanded.is_empty()));
    // The view menu's commands do the same.
    desk.update_in(cx, |desk, window, cx| {
        desk.execute("expand-sessions", window, cx)
    });
    draw(cx);
    assert!(shown(cx, "pane-@1") && shown(cx, "catalog-pane-mini-@1"));
    // Folding forgets sessions the list cannot show, so none returns open.
    desk.update(cx, |desk, cx| {
        desk.update_prefs(
            |prefs| prefs.set_sidebar_host_visible(Some("mini".into()), false),
            cx,
        )
    });
    desk.update_in(cx, |desk, window, cx| {
        desk.execute("collapse-sessions", window, cx)
    });
    desk.update(cx, |desk, cx| {
        desk.update_prefs(|prefs| prefs.sidebar_hidden_hosts.clear(), cx)
    });
    draw(cx);
    assert!(!shown(cx, "pane-@1") && !shown(cx, "catalog-pane-mini-@1"));
    desk.read_with(cx, |desk, _| assert!(desk.expanded.is_empty()));
    // Away from the session, its same-labelled terminals keep their ids.
    click_on(cx, "work-mini-8");
    desk.update_in(cx, |desk, window, cx| {
        desk.execute("expand-sessions", window, cx)
    });
    draw(cx);
    assert!(shown(cx, "catalog-pane-suffix-Local-@1") && shown(cx, "catalog-pane-suffix-Local-@2"));
}

#[gpui::test]
fn only_the_caret_changes_disclosure_and_equal_names_on_other_hosts_stay_open(
    cx: &mut TestAppContext,
) {
    let _dial = DeterministicDial::arm();
    // Folding at once: with motion, the rows stay drawn while they spring shut.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (handle, desk, _) = launch(cx);
    let cx = &mut gpui::VisualTestContext::from_window(*handle, cx);
    desk.update(cx, |desk, cx| {
        install(desk, with_pane(connection(1, vec![session(7, "main")])));
        observe(
            &mut desk.fleet,
            Some("mini"),
            with_pane(connection(2, vec![session(8, "main")])),
        );
        desk.hosts.push(super::super::host::RemoteHost {
            name: "mini".into(),
            endpoint: "quic://fixture.invalid:1".into(),
        });
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds("pane-@1").is_none(),
        "sessions start folded"
    );
    click_on(cx, "disclose-work-Local-7");
    click_on(cx, "disclose-work-mini-8");
    assert!(cx.debug_bounds("catalog-pane-mini-@1").is_some());
    let local = cx.debug_bounds("work-Local-7").unwrap();
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), gpui::Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("catalog-pane-mini-@1").is_none());
    assert!(
        cx.debug_bounds("pane-@1").is_some(),
        "same-named local session stays expanded"
    );
    assert_eq!(cx.debug_bounds("work-Local-7").unwrap(), local);
    let expanded = desk.read_with(cx, |desk, _| {
        assert!(desk.remote.is_none());
        desk.expanded.clone()
    });
    let remote = cx.debug_bounds("work-mini-8").unwrap();
    cx.simulate_click(remote.center(), gpui::Modifiers::none());
    draw(cx);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.remote.as_deref(), Some("mini"));
        assert_eq!(desk.expanded, expanded);
    });
    let caret = cx.debug_bounds("disclose-work-mini-8").unwrap();
    cx.simulate_click(caret.center(), gpui::Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("pane-@1").is_some(),
        "the promoted host is already active"
    );
    assert!(
        cx.debug_bounds("catalog-pane-Local-@1").is_some(),
        "the other host remains expanded"
    );
}
