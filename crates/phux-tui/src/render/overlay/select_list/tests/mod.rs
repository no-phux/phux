#![allow(clippy::expect_used, reason = "tests")]

use super::paint::item_line;
use super::*;
use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::ModSet;
use ratatui::style::Modifier;
use std::collections::BTreeMap;

fn action(name: &str) -> ResolvedAction {
    crate::attach::resolved(name, std::collections::BTreeMap::new())
}

use crate::test_util::press_text as press;

fn key(sl: &mut SelectList, key: PhysicalKey) -> OverlayCommand {
    sl.handle_key(&press(key, None))
}

fn type_str(sl: &mut SelectList, s: &str) {
    for ch in s.chars() {
        sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
    }
}

/// The action Enter commits, or `None` when it commits nothing.
fn commit(sl: &mut SelectList) -> Option<String> {
    match key(sl, PhysicalKey::Enter) {
        OverlayCommand::Commit(a) => Some(a.action),
        _ => None,
    }
}

fn labels(sl: &SelectList) -> Vec<&str> {
    sl.filtered_indices()
        .into_iter()
        .map(|i| sl.items[i].label.as_str())
        .collect()
}

fn list(items: Vec<SelectItem>) -> SelectList {
    SelectList::new("command palette", items, &Theme::default())
}

fn sample() -> SelectList {
    list(vec![
        SelectItem::new("split-pane", action("split-pane")).secondary("C-a |"),
        SelectItem::new("new-window", action("new-window")).secondary("C-a c"),
        SelectItem::new("detach", action("detach")).secondary("C-a d"),
    ])
}

fn grouped() -> SelectList {
    list(vec![
        SelectItem::header("Pane"),
        SelectItem::new("split-pane", action("split-pane")).indented(),
        SelectItem::header("Window"),
        SelectItem::new("new-window", action("new-window")).indented(),
    ])
}

/// `n` rows labelled `item-0 ..= item-(n-1)`.
fn long_list(n: usize) -> SelectList {
    list(
        (0..n)
            .map(|i| SelectItem::new(format!("item-{i}"), action(&format!("act-{i}"))))
            .collect(),
    )
}

fn render_buf(sl: &SelectList, w: u16, h: u16) -> Buffer {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    sl.render(area, &mut buf);
    buf
}

use crate::test_util::rendered as render_to_string;

/// The label of the row painted on the selection surface, or `None` when
/// no row is highlighted on screen (the cursor walked off the box).
fn painted_selection(sl: &SelectList, w: u16, h: u16) -> Option<String> {
    let buf = render_buf(sl, w, h);
    (0..h).find_map(|y| {
        let row: String = (0..w)
            .filter(|&x| buf[(x, y)].bg == sl.theme.selection_bg)
            .map(|x| buf[(x, y)].symbol())
            .collect();
        let row = row.trim();
        row.starts_with("item-")
            .then(|| row.split_whitespace().next().unwrap_or_default().to_owned())
    })
}

/// The visible row the cursor is on.
fn selected(sl: &SelectList) -> usize {
    sl.cursor_row(&sl.filtered_indices())
        .expect("a selected row")
}

/// The item the cursor is on.
fn cursor(sl: &SelectList) -> usize {
    sl.model.cursor.expect("a cursor")
}

fn painted_row(sl: &SelectList, idx: usize, width: u16) -> String {
    item_line(&sl.theme, &sl.items[idx], None, false, width)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

#[test]
fn a_query_matching_nothing_says_no_matches_in_the_footer() {
    let mut sl = sample();
    type_str(&mut sl, "zzzz");
    let out = render_to_string(&sl, 60, 12);
    assert!(out.contains("no matches"), "{out}");
    assert!(!out.contains("0 rows"), "{out}");
}

#[test]
fn fuzzy_score_matches_ordered_subsequences_and_ranks_them() {
    assert_eq!(fuzzy_score("", "anything"), Some(0));
    assert!(fuzzy_score("spn", "split-pane").is_some());
    assert!(fuzzy_score("nw", "new-window").is_some());
    assert_eq!(fuzzy_score("zzz", "new-window"), None);
    assert_eq!(fuzzy_score("wen", "new-window"), None, "order matters");
    // A contiguous front-anchored run beats a scattered match, and a
    // word-boundary hit beats a mid-word one.
    assert!(fuzzy_score("sp", "split-pane") > fuzzy_score("sp", "previous-pane"));
    assert!(fuzzy_score("p", "split-pane") > fuzzy_score("p", "copy-mode"));
}

#[test]
fn session_management_legend_names_tab() {
    let sl = list(vec![SelectItem::new("a", action("switch-session"))]).with_session_management();
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    sl.render(Rect::new(0, 0, 80, 24), &mut buf);
    let text: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(text.contains("tab manage"), "{text}");
}

#[test]
fn clip_left_keeps_the_tail() {
    assert_eq!(clip_left("/a/b", 10), "/a/b");
    assert_eq!(clip_left("/usr/local/share", 8), "…l/share");
}

#[test]
fn session_management_tab_preserves_filtered_target_and_ignores_status_rows() {
    let target = crate::attach::resolved(
        "switch-session",
        std::collections::BTreeMap::from([
            ("id".to_owned(), toml::Value::Integer(42)),
            ("name".to_owned(), toml::Value::String("build".to_owned())),
        ]),
    );
    let mut other = target.clone();
    other.args.insert("id".to_owned(), toml::Value::Integer(41));
    let mut sl = list(vec![
        SelectItem::header("Local"),
        SelectItem::new("build", other),
        SelectItem::new("build", target.clone()),
    ])
    .with_session_management();
    type_str(&mut sl, "build");
    key(&mut sl, PhysicalKey::ArrowDown);
    let before = selected(&sl);
    let OverlayCommand::RunAbove(manage) = key(&mut sl, PhysicalKey::Tab) else {
        panic!("management")
    };
    assert_eq!(manage.action, "session-actions");
    let mut expected_args = target.args;
    expected_args.insert(
        "switch-action".to_owned(),
        toml::Value::String("switch-session".to_owned()),
    );
    assert_eq!(manage.args, expected_args);
    assert_eq!(sl.query(), "build", "drill-down preserves search");
    assert_eq!(selected(&sl), before, "drill-down preserves cursor");
    sl.replace_items(vec![SelectItem::new("build", action("switch-host"))]);
    assert_eq!(
        key(&mut sl, PhysicalKey::Tab),
        OverlayCommand::Stay,
        "host availability isn't a session"
    );
    let mut static_list = sample();
    assert_eq!(
        key(&mut static_list, PhysicalKey::Tab),
        OverlayCommand::Stay
    );
}

#[test]
fn filtering_ranks_best_first_and_keeps_ties_in_source_order() {
    let mut sl = list(vec![
        SelectItem::new("toggle-sidebar", action("toggle-sidebar")),
        SelectItem::new("split-pane", action("split-pane")),
        SelectItem::new("previous-pane", action("previous-pane")),
    ]);
    type_str(&mut sl, "sp");
    assert_eq!(labels(&sl)[0], "split-pane");

    let mut sl = list(vec![
        SelectItem::new("x-alpha", action("a")),
        SelectItem::new("x-bravo", action("b")),
    ]);
    type_str(&mut sl, "x");
    assert_eq!(labels(&sl), vec!["x-alpha", "x-bravo"]);

    // The secondary (a chord) is filter text too; typed `j` is text once
    // the query is non-empty.
    let mut sl = sample();
    type_str(&mut sl, "nw");
    assert_eq!(labels(&sl), vec!["new-window"]);
    let mut sl = sample();
    sl.handle_key(&press(PhysicalKey::A, Some("d")));
    sl.handle_key(&press(PhysicalKey::J, Some("j")));
    assert_eq!(sl.query(), "dj");
    assert!(labels(&sl).is_empty());
}

#[test]
fn headers_group_results_and_follow_their_best_row() {
    let mut sl = grouped();
    assert_eq!(sl.filtered_indices(), vec![0, 1, 2, 3]);
    // Opens on the first selectable row; Down skips the next header.
    assert_eq!(commit(&mut sl).as_deref(), Some("split-pane"));
    key(&mut sl, PhysicalKey::ArrowDown);
    assert_eq!(commit(&mut sl).as_deref(), Some("new-window"));
    // A typed query keeps the bands: each match under its own header, and
    // the band holding the best match first.
    type_str(&mut sl, "window");
    assert_eq!(sl.filtered_indices(), vec![2, 3], "only Window's band matches");
    let mut sl = grouped();
    type_str(&mut sl, "e");
    let shown = sl.filtered_indices();
    assert!(sl.items[shown[0]].is_header(), "a band leads: {shown:?}");
    assert_eq!(shown.len(), 4, "both bands, each under its header");
    assert!(commit(&mut sl).is_some(), "the cursor rests on a row, never a band");

    // A header-only list, a trailing header under End, and an empty
    // filter never commit a placeholder.
    assert_eq!(commit(&mut list(vec![SelectItem::header("Pane")])), None);
    let mut sl = list(vec![
        SelectItem::new("only-item", action("only")),
        SelectItem::header("Empty session"),
    ]);
    key(&mut sl, PhysicalKey::End);
    assert_eq!(commit(&mut sl).as_deref(), Some("only"));
    let mut sl = sample();
    type_str(&mut sl, "zzz");
    assert_eq!(commit(&mut sl), None);
}

#[test]
fn keys_navigate_saturate_and_dismiss() {
    let mut sl = sample();
    let ctrl = |k| {
        let mut ev = press(k, None);
        ev.mods = ModSet::CTRL;
        ev
    };
    for _ in 0..3 {
        sl.handle_key(&ctrl(PhysicalKey::N));
    }
    assert_eq!(commit(&mut sl).as_deref(), Some("detach"), "no wrap");
    sl.handle_key(&ctrl(PhysicalKey::P));
    assert_eq!(commit(&mut sl).as_deref(), Some("new-window"));
    let mut sl = sample();
    sl.handle_key(&press(PhysicalKey::J, Some("j")));
    assert_eq!(
        commit(&mut sl).as_deref(),
        Some("new-window"),
        "j on an empty query"
    );
    assert_eq!(key(&mut sl, PhysicalKey::Escape), OverlayCommand::Dismiss);
}

/// The reported bug: walking the cursor down a long list marched it off
/// the bottom of the box. The viewport follows the selection both ways.
#[test]
fn refresh_needs_the_live_key_and_keeps_query_and_target() {
    let fresh = vec![SelectItem::new("fresh-row", action("fresh"))];
    assert!(
        !sample().refresh_items("agent-fleet", &fresh),
        "static list"
    );
    assert!(
        !sample()
            .with_live_key("other")
            .refresh_items("agent-fleet", &fresh)
    );
    let mut sl = sample().with_live_key("agent-fleet");
    assert!(sl.refresh_items("agent-fleet", &fresh));
    assert_eq!(labels(&sl), vec!["fresh-row"]);

    // The query survives and keeps filtering the new rows.
    let mut sl = sample().with_live_key("agent-fleet");
    type_str(&mut sl, "det");
    sl.refresh_items(
        "agent-fleet",
        &[
            SelectItem::new("detach-me", action("a")),
            SelectItem::new("other", action("b")),
        ],
    );
    assert_eq!(labels(&sl), vec!["detach-me"]);
    assert_eq!(commit(&mut sl).as_deref(), Some("a"));

    // Rows inserted above the cursor: the selection follows the action.
    let mut sl = list(vec![
        SelectItem::new("work", action("work")),
        SelectItem::new("scratch", action("scratch")),
    ])
    .with_live_key("session-picker");
    key(&mut sl, PhysicalKey::ArrowDown);
    sl.refresh_items(
        "session-picker",
        &[
            SelectItem::header("This host"),
            SelectItem::new("work", action("work")),
            SelectItem::new("scratch", action("scratch")),
            SelectItem::header("edge"),
            SelectItem::new("build", action("build")),
        ],
    );
    assert_eq!(commit(&mut sl).as_deref(), Some("scratch"));

    // The action is gone: fall back to the same position, snapped and
    // clamped onto a selectable row.
    sl.refresh_items(
        "session-picker",
        &[
            SelectItem::new("c", action("c")),
            SelectItem::new("d", action("d")),
        ],
    );
    assert_eq!(selected(&sl), 1);
    sl.refresh_items("session-picker", &[SelectItem::new("only", action("only"))]);
    assert_eq!(commit(&mut sl).as_deref(), Some("only"));
}

/// phux-kmlrz: a live rename changes the display name inside the action.
/// Selection stays on that session's host-qualified id, including when a
/// same-numbered session on another host is listed first.
#[test]
fn a_live_rename_keeps_the_host_qualified_session() {
    let mut sl = list(vec![
        SelectItem::new("alpha", session_switch("alpha", 1, None, None)),
        SelectItem::new("beta", session_switch("beta", 2, None, None)),
        SelectItem::new("gamma", session_switch("gamma", 3, None, None)),
    ])
    .with_live_key("session-picker");
    key(&mut sl, PhysicalKey::ArrowDown);

    sl.refresh_items(
        "session-picker",
        &[
            SelectItem::new("other", session_switch("other", 2, Some("edge"), None)),
            SelectItem::new("alpha", session_switch("alpha", 1, None, None)),
            SelectItem::new("gamma", session_switch("gamma", 3, None, None)),
            SelectItem::new("zzz", session_switch("zzz", 2, None, None)),
        ],
    );
    assert_eq!(sl.items[cursor(&sl)].label, "zzz");
    let kept = &sl.items[cursor(&sl)].action.args;
    assert_eq!(kept.get("id"), Some(&toml::Value::Integer(2)));
    assert!(!kept.contains_key("host"));
}

/// Changing anything other than the display name is not a rename. The
/// bare session id must not pull the highlight onto a different row.
#[test]
fn a_non_rename_refresh_keeps_the_selected_position() {
    let mut sl = list(vec![
        SelectItem::new("alpha", session_switch("alpha", 1, None, Some(0))),
        SelectItem::new("beta", session_switch("beta", 2, None, Some(1))),
        SelectItem::new("gamma", session_switch("gamma", 3, None, Some(0))),
    ])
    .with_live_key("session-picker");
    key(&mut sl, PhysicalKey::ArrowDown);
    sl.refresh_items(
        "session-picker",
        &[
            SelectItem::new("beta", session_switch("beta", 2, None, None)),
            SelectItem::new("alpha", session_switch("alpha", 1, None, Some(0))),
            SelectItem::new("shifted", session_switch("shifted", 9, None, Some(1))),
            SelectItem::new("gamma", session_switch("gamma", 3, None, Some(0))),
        ],
    );
    assert_eq!(selected(&sl), 1);
    assert_eq!(sl.items[cursor(&sl)].label, "alpha");
}

fn session_switch(name: &str, id: i64, host: Option<&str>, window: Option<i64>) -> ResolvedAction {
    let mut args = BTreeMap::new();
    args.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    args.insert("id".to_owned(), toml::Value::Integer(id));
    if let Some(host) = host {
        args.insert("host".to_owned(), toml::Value::String(host.to_owned()));
    }
    if let Some(window) = window {
        args.insert("window".to_owned(), toml::Value::Integer(window));
    }
    crate::attach::resolved("switch-session", args)
}

mod render;

/// Shift, Ctrl or Alt held on Enter still commits the highlighted row.
#[test]
fn a_modified_enter_commits() {
    for mods in [ModSet::SHIFT, ModSet::CTRL, ModSet::ALT] {
        let mut sl = sample();
        let enter = crate::test_util::key_event(PhysicalKey::Enter, mods, None);
        let OverlayCommand::Commit(a) = sl.handle_key(&enter) else {
            panic!("{mods:?}+Enter commits");
        };
        assert_eq!(a.action, "split-pane");
    }
}
