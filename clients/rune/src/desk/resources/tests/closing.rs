//! Where the keyboard is once a pane or a tab has closed.

use super::*;
use phux_client_runtime::control::Event;
use phux_protocol::wire::frame::CloseReason;

/// Two panes in the first tab, `3` alone in the second; `2` has the keyboard.
fn two_tabs() -> (Arc<Client>, Runtime<Desk>) {
    let records = mirrors([ResourceKind::Terminal; 3]);
    let mut arrangement = doc(split(1, 2));
    arrangement.add_group("other".to_owned(), id(3));
    arrangement.active = 0;
    let (client, mut rt) = fixture(&records, &arrangement);
    rt.update(|d, _| {
        d.settings.tab_bar = phux_config_core::rune::TabBar::Top;
        d.focus_pane(&id(2));
    });
    step(&mut rt);
    assert_eq!(focus(&rt), Some(id(2)));
    (client, rt)
}

fn focus(rt: &Runtime<Desk>) -> Option<ResourceId> {
    let shown = rt.state.connection.shown.as_ref();
    shown.map(|shown| shown.resource.clone())
}

fn press_close(rt: &mut Runtime<Desk>, tab: usize) {
    let key = format!("tab-close-{tab}");
    let (x, y) = rt.find(&key).expect("the tab's close");
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0);
    step(rt);
}

fn ended(n: u32) -> Event {
    Event::TerminalClosed {
        terminal_id: id(n),
        exit_status: Some(0),
        signal: None,
        reason: CloseReason::Unknown,
    }
}

fn closed(rt: &mut Runtime<Desk>, client: &Client, n: u32) {
    rt.update(|d, tx| {
        d.apply(client, ended(n), tx);
    });
    step(rt);
}

#[test]
fn closing_another_tab_leaves_the_focused_pane_alone() {
    let (client, mut rt) = two_tabs();
    press_close(&mut rt, 1);
    assert_eq!(rt.state.pending, None);
    assert_eq!(focus(&rt), Some(id(2)), "the press moves nothing");
    closed(&mut rt, &client, 3);
    assert_eq!(focus(&rt), Some(id(2)), "nor does the close");
}

#[test]
fn a_tab_that_asks_before_closing_goes_back_to_the_pane_used_before_it() {
    let (client, mut rt) = two_tabs();
    rt.update(|d, _| d.settings.confirm_close = true);
    press_close(&mut rt, 1);
    assert!(rt.state.pending.is_some());
    assert_eq!(focus(&rt), Some(id(3)), "the question shows its tab");
    rt.update(|d, _| d.confirm_pending());
    closed(&mut rt, &client, 3);
    assert_eq!(focus(&rt), Some(id(2)), "not the server's pane");
}

#[test]
fn a_split_tab_that_closes_whole_goes_back_to_the_tab_used_before_it() {
    for order in [[3, 4], [4, 3]] {
        let records = mirrors([ResourceKind::Terminal; 4]);
        let mut arrangement = doc(split(1, 2));
        arrangement.add_group("other".to_owned(), id(3));
        arrangement.groups[1].state.tree = Some(split(3, 4));
        arrangement.active = 0;
        let (client, mut rt) = fixture(&records, &arrangement);
        rt.update(|d, _| d.focus_pane(&id(2)));
        step(&mut rt);
        rt.update(|d, _| d.select_tab(1));
        step(&mut rt);
        assert_eq!(focus(&rt), Some(id(3)));
        // Both ends arrive before the window looks again.
        for n in order {
            rt.update(|d, tx| {
                d.apply(&client, ended(n), tx);
            });
        }
        step(&mut rt);
        assert_eq!(focus(&rt), Some(id(2)), "{order:?}");
    }
}
