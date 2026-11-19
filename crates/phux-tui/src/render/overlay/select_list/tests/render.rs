//! How a list paints: the viewport, the scrollbar, rows, and clicks.

use super::*;

#[test]
fn the_viewport_follows_the_selection() {
    let mut sl = long_list(40);
    for _ in 0..20 {
        key(&mut sl, PhysicalKey::ArrowDown);
    }
    assert_eq!(painted_selection(&sl, 40, 16).as_deref(), Some("item-20"));
    assert!(sl.model.scroll.get() > 0);
    for _ in 0..20 {
        key(&mut sl, PhysicalKey::ArrowUp);
    }
    let text = render_to_string(&sl, 40, 16);
    assert_eq!(sl.model.scroll.get(), 0);
    assert!(text.contains("item-0"), "{text}");

    // A narrowing filter rewinds a stranded offset.
    for _ in 0..30 {
        key(&mut sl, PhysicalKey::ArrowDown);
    }
    render_to_string(&sl, 40, 16);
    type_str(&mut sl, "item-7");
    let text = render_to_string(&sl, 40, 16);
    assert_eq!(sl.model.scroll.get(), 0);
    assert!(text.contains("item-7"), "{text}");

    // Only an overflowing list paints a scrollbar.
    assert!(render_to_string(&long_list(40), 40, 16).contains('┃'));
    assert!(!render_to_string(&long_list(3), 40, 16).contains('┃'));
}

/// The scrollbar runs beside the list rows only: the thumb starts on the
/// first list row, and the bottom border keeps its corner.
#[test]
fn scrollbar_spans_the_list_rows_and_spares_the_border() {
    let sl = long_list(40);
    let area = Rect::new(0, 0, 40, 16);
    let modal = sl.modal_area(area);
    let buf = render_buf(&sl, 40, 16);
    let right = modal.x + modal.width - 1;
    let bottom = modal.y + modal.height - 1;
    assert_eq!(
        buf[(right, modal.y + 2)].symbol(),
        "┃",
        "thumb on the first list row"
    );
    assert_eq!(
        buf[(right, bottom)].symbol(),
        "╯",
        "bottom-right corner survives"
    );
}

#[test]
fn page_home_end_and_wheel_move_the_selection() {
    let mut sl = long_list(40);
    // 40x16 is compact: full-bleed, 16 - 3 chrome rows = 13 visible.
    render_to_string(&sl, 40, 16);
    key(&mut sl, PhysicalKey::PageDown);
    assert_eq!(selected(&sl), 13, "a page is the visible rows");
    key(&mut sl, PhysicalKey::PageUp);
    assert_eq!(selected(&sl), 0);
    key(&mut sl, PhysicalKey::End);
    assert_eq!(selected(&sl), 39);
    assert_eq!(painted_selection(&sl, 40, 16).as_deref(), Some("item-39"));
    key(&mut sl, PhysicalKey::Home);
    assert_eq!(selected(&sl), 0);

    let wheel = |button| MouseEvent {
        action: MouseAction::Press,
        button,
        mods: ModSet::empty(),
        x: 0.0,
        y: 0.0,
    };
    assert_eq!(
        sl.handle_mouse(&wheel(MouseButton::Five)),
        OverlayCommand::Stay
    );
    assert_eq!(selected(&sl), WHEEL_SCROLL_ROWS);
    sl.handle_mouse(&wheel(MouseButton::Four));
    sl.handle_mouse(&wheel(MouseButton::Four));
    assert_eq!(selected(&sl), 0, "saturates at the top");

    // Wheeling far down scrolls the view with the selection.
    for _ in 0..8 {
        sl.handle_mouse(&wheel(MouseButton::Five));
    }
    assert_eq!(painted_selection(&sl, 40, 16).as_deref(), Some("item-24"));
    assert!(sl.model.scroll.get() > 0);
}

/// The cell of the first painted occurrence of `needle`.
fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
    let area = buf.area;
    (0..area.height)
        .find_map(|y| {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            row.find(needle)
                .map(|byte| (u16::try_from(row[..byte].chars().count()).expect("fits"), y))
        })
        .unwrap_or_else(|| panic!("{needle} is painted"))
}

fn click(sl: &mut SelectList, x: u16, y: u16) -> OverlayCommand {
    let at = |action| MouseEvent {
        action,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    };
    assert_eq!(
        sl.handle_mouse(&at(MouseAction::Press)),
        OverlayCommand::Stay
    );
    sl.handle_mouse(&at(MouseAction::Release))
}

/// A click commits the row under the pointer; a click on a header, the
/// query line, or outside the box commits nothing.
#[test]
fn clicking_a_row_commits_it() {
    let mut sl = grouped();
    let buf = render_buf(&sl, 100, 30);
    let (x, y) = find(&buf, "new-window");
    match click(&mut sl, x, y) {
        OverlayCommand::Commit(a) => assert_eq!(a.action, "new-window"),
        other => panic!("expected a commit, got {other:?}"),
    }
    let (x, y) = find(&buf, "Window");
    assert_eq!(click(&mut sl, x, y), OverlayCommand::Stay, "header");
    let (x, y) = find(&buf, ">");
    assert_eq!(click(&mut sl, x + 2, y), OverlayCommand::Stay, "query");
    assert_eq!(click(&mut sl, 0, 0), OverlayCommand::Stay, "outside");
    assert_eq!(
        commit(&mut sl).as_deref(),
        Some("new-window"),
        "the clicked row holds the cursor"
    );
}

/// The query is a ratcn field: the caret moves, and typing lands at it.
#[test]
fn the_query_edits_at_its_caret() {
    let mut sl = sample();
    type_str(&mut sl, "dch");
    key(&mut sl, PhysicalKey::ArrowLeft);
    key(&mut sl, PhysicalKey::ArrowLeft);
    type_str(&mut sl, "eta");
    assert_eq!(sl.query(), "detach");
    assert_eq!(labels(&sl), vec!["detach"]);
    key(&mut sl, PhysicalKey::End);
    assert_eq!(sl.query(), "detach", "End is the list's, not the caret's");
    sl.handle_paste("zz\nq");
    assert_eq!(
        sl.query(),
        "detazz qch",
        "a paste is one line, at the caret"
    );
    assert_eq!(commit(&mut sl), None);
}

/// Pins the mid-scroll box: windowed rows, the selection inside it, and
/// the scrollbar thumb away from both ends.
#[test]
fn scrolled_list_render_is_stable() {
    let mut sl = long_list(24);
    render_to_string(&sl, 44, 16);
    for _ in 0..15 {
        key(&mut sl, PhysicalKey::ArrowDown);
    }
    insta::assert_snapshot!(render_to_string(&sl, 44, 16));
}

/// Above the compact breakpoint the picker floats: a centered box with
/// panes visible around it, not a screen, and only as tall as its rows
/// need (three rows: the query line, the rows, and the two borders).
#[test]
fn roomy_viewport_render_still_floats() {
    let sl = sample();
    let area = Rect::new(0, 0, 100, 30);
    let bounds = sl.bounds(area).expect("bounded");
    assert_eq!((bounds.width, bounds.height), (50, 6));
    assert!(bounds.x > 0 && bounds.y > 0);
    insta::assert_snapshot!(render_to_string(&sl, 100, 30));
}

/// A long list stops at half the viewport and scrolls; a list that
/// refreshes to fewer rows keeps the box it had.
#[test]
fn box_height_caps_at_half_and_never_shrinks() {
    let area = Rect::new(0, 0, 100, 30);
    let long = long_list(40);
    assert_eq!(long.bounds(area).expect("bounded").height, 15);
    let mut live = long_list(8).with_live_key("agent-fleet");
    assert_eq!(live.bounds(area).expect("bounded").height, 11);
    assert!(live.refresh_items("agent-fleet", &[SelectItem::header("Searching…")]));
    assert_eq!(live.bounds(area).expect("bounded").height, 11);
}

#[test]
fn attention_rows_paint_hot_and_selection_owns_its_colors() {
    let theme = Theme::default();
    let sl = SelectList::new(
        "agent fleet",
        vec![
            SelectItem::new("calm", action("a")),
            SelectItem::new("needs-you", action("b")).attention(),
        ],
        &theme,
    );
    assert_eq!(
        item_line(&theme, &sl.items[0], None, false, 40).spans[0]
            .style
            .fg,
        Some(theme.text)
    );
    let hot = item_line(&theme, &sl.items[1], None, false, 40);
    assert_eq!(hot.spans[0].style.fg, Some(theme.attention));
    assert!(hot.spans[0].style.add_modifier.contains(Modifier::BOLD));
    let selected = item_line(&theme, &sl.items[1], None, true, 40);
    assert_eq!(selected.spans[0].style.fg, Some(theme.selection_fg));
    assert_eq!(selected.spans[0].style.bg, Some(theme.selection_bg));
}

/// Every row, at every width, measures exactly the interior width (a
/// long row once painted through the modal border onto the pane behind).
#[test]
fn a_row_never_overruns_the_modal_interior() {
    let sl = list(vec![
        SelectItem::new(
            "a-very-long-window-name-that-will-not-fit-anywhere",
            action("x"),
        )
        .secondary("~/some/deeply/nested/working/directory  feature/branch"),
        SelectItem::new("构建工具", action("x")).secondary("工作目录/main"),
        SelectItem::new("cafe\u{301}", action("x"))
            .secondary("re\u{301}vision")
            .indented(),
    ]);
    for width in 1u16..=80 {
        for item in sl.items.iter() {
            for selected in [false, true] {
                let painted: String = item_line(&sl.theme, item, None, selected, width)
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                assert_eq!(
                    crate::render::display_width(&painted),
                    usize::from(width),
                    "{width}: {painted:?}"
                );
            }
        }
    }
}

/// The label is the row's identity: the secondary yields first, unless it
/// is a short chord, which survives whole while a long label clips.
#[test]
fn narrow_rows_keep_the_label_and_short_chords() {
    let sl = list(vec![
        SelectItem::new("builder", action("x")).secondary("working - main"),
        SelectItem::new(
            "Zoom the focused pane to fill the whole window",
            action("z"),
        )
        .secondary("C-a z"),
    ]);
    assert_eq!(painted_row(&sl, 0, 30), "builder         working - main");
    assert!(painted_row(&sl, 0, 16).starts_with("builder"));
    assert_eq!(painted_row(&sl, 0, 7), "builder");
    let chord = painted_row(&sl, 1, 40);
    assert!(
        chord.ends_with("C-a z") && chord.contains(crate::render::ELLIPSIS),
        "{chord:?}"
    );
}

/// A query wider than the field scrolls to keep its tail (the caret)
/// in view, without changing what it filters.
#[test]
fn long_query_keeps_its_tail_without_changing_the_filter() {
    let mut sl = long_list(3);
    let query = "item-2-and-a-long-tail-of-words-xyz";
    type_str(&mut sl, query);
    assert_eq!(sl.query(), query);
    let out = render_to_string(&sl, 40, 16);
    assert!(out.contains("words-xyz"), "{out}");
    assert!(!out.contains("item-2-and"), "{out}");
}
