use super::rows::relative;
use super::*;
use crate::test_util::press_text;
use phux_protocol::wire::frame::{PathKind, PathResults, PathRow, PathStatus};

fn painted(picker: &PathPicker, area: Rect) -> Vec<String> {
    let mut buf = Buffer::empty(area);
    picker.render(area, &mut buf);
    (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Hand `picker` a truncated browse of `/home` with thirty files.
fn browse_home(picker: &mut PathPicker) {
    picker.update_paths(&Ok(PathResults {
        root: "/home".into(),
        parent: Some("/".into()),
        rows: (0..30)
            .map(|i| PathRow {
                path: format!("/home/f{i:02}"),
                kind: PathKind::File,
            })
            .collect(),
        status: PathStatus::Truncated,
    }));
}

fn committed_path(command: OverlayCommand) -> Option<(String, String)> {
    let OverlayCommand::Commit(action) = command else {
        return None;
    };
    let path = action.args["path"].as_str()?.to_owned();
    Some((action.action, path))
}

#[test]
fn footer_status_keeps_the_deepest_directory() {
    let long = "/private/tmp/claude-501/-Users-phall-workspace-phux/deepest";
    let picker = PathPicker::new(long.to_owned(), &Theme::default());
    let text = painted(&picker, Rect::new(0, 0, 60, 20)).join("\n");
    assert!(text.contains("Insert path"));
    assert!(text.contains('…') && text.contains("deepest"), "{text}");
    assert!(!text.contains("Insert path:"));
}

#[test]
fn browse_rows_insert_and_navigate_without_spawning() {
    let result = Ok(PathResults {
        root: "/home".into(),
        parent: Some("/".into()),
        rows: vec![PathRow {
            path: "/home/a b".into(),
            kind: PathKind::Directory,
        }],
        status: PathStatus::Complete,
    });
    let items = rows(&result, true);
    let actions: Vec<&str> = items
        .iter()
        .map(|i| i.action.as_ref().expect("a choosable row").action.as_str())
        .collect();
    assert_eq!(
        actions,
        ["find-path", "insert-path", "find-path", "insert-path"]
    );
    assert_eq!(
        items[3].action.as_ref().expect("a row").args["path"].as_str(),
        Some("/home/a b")
    );
    assert_eq!(
        items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
        ["../", "./", "open a b/", "a b/"],
        "rows name entries relative to the root"
    );
}

#[test]
fn rows_outside_or_at_the_root_keep_their_full_path() {
    assert_eq!(relative("/home/a", "/home"), "a");
    assert_eq!(relative("/etc/x", "/"), "etc/x");
    assert_eq!(relative("/homework/a", "/home"), "/homework/a");
    assert_eq!(relative("/home", "/home"), "/home");
}

#[test]
fn host_paths_with_controls_are_painted_escaped_and_kept_verbatim() {
    let result = Ok(PathResults {
        root: "/tmp".into(),
        parent: None,
        rows: vec![PathRow {
            path: "/tmp/x\u{1b}]0;pwn\u{7}\ny".into(),
            kind: PathKind::File,
        }],
        status: PathStatus::Complete,
    });
    let items = rows(&result, false);
    assert!(
        !items[0].label.chars().any(char::is_control),
        "{:?}",
        items[0].label
    );
    assert_eq!(
        items[0].action.as_ref().expect("a row").args["path"].as_str(),
        Some("/tmp/x\u{1b}]0;pwn\u{7}\ny"),
        "the action keeps the original so insertion can refuse it"
    );
    let refused = Err(phux_protocol::wire::frame::PathQueryError {
        root: "/\u{1b}[2J".into(),
        code: phux_protocol::wire::frame::PathErrorCode::Other,
        message: "bad\u{9b}".into(),
    });
    let note = &rows(&refused, true)[0];
    assert!(!note.label.chars().any(char::is_control));
    assert!(note.action.is_none(), "an error commits nothing");
}

#[test]
fn typing_a_query_clears_old_rows_before_recursive_reply() {
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    browse_home(&mut picker);
    let key = press_text(PhysicalKey::X, Some("x"));
    assert_eq!(picker.handle_key(&key), OverlayCommand::Stay);
    assert_eq!(picker.path_search(), Some(("/home", "x")));
    assert_eq!(
        picker.handle_key(&press_text(PhysicalKey::Enter, None)),
        OverlayCommand::Stay,
        "an old result must not be committed while the host is searching"
    );
    // A paste is a query edit too.
    browse_home(&mut picker);
    picker.handle_paste("y\u{7}");
    assert_eq!(picker.path_search(), Some(("/home", "xy")));
    assert_eq!(
        picker.handle_key(&press_text(PhysicalKey::Enter, None)),
        OverlayCommand::Stay
    );
}

#[test]
fn the_first_j_or_k_is_search_text_not_navigation() {
    let mut picker = PathPicker::new("/src".into(), &Theme::default());
    browse_home(&mut picker);
    picker.handle_key(&press_text(PhysicalKey::J, Some("j")));
    picker.handle_key(&press_text(PhysicalKey::K, Some("k")));
    assert_eq!(picker.path_search(), Some(("/src", "jk")));
}

/// Enter commits the highlighted row; arrows, `C-n`/`C-p`, and the wheel
/// move it over choosable rows only, saturating at both ends.
#[test]
fn keys_and_the_wheel_move_the_highlight_and_enter_commits_it() {
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    browse_home(&mut picker);
    let enter = press_text(PhysicalKey::Enter, None);
    assert_eq!(
        committed_path(picker.handle_key(&enter)),
        Some(("find-path".to_owned(), "/".to_owned())),
        "the first choosable row is the parent"
    );
    picker.handle_key(&press_text(PhysicalKey::ArrowUp, None));
    picker.handle_key(&press_text(PhysicalKey::ArrowDown, None));
    picker.handle_key(&crate::test_util::key_event(
        PhysicalKey::N,
        ModSet::CTRL,
        None,
    ));
    assert_eq!(
        committed_path(picker.handle_key(&enter)),
        Some(("insert-path".to_owned(), "/home/f00".to_owned()))
    );
    let wheel = |button| MouseEvent {
        action: MouseAction::Press,
        button,
        mods: ModSet::empty(),
        x: 0.0,
        y: 0.0,
    };
    picker.handle_mouse(&wheel(MouseButton::Five));
    assert_eq!(
        committed_path(picker.handle_key(&enter)),
        Some(("insert-path".to_owned(), "/home/f03".to_owned()))
    );
    for _ in 0..40 {
        picker.handle_key(&press_text(PhysicalKey::PageDown, None));
    }
    assert_eq!(
        committed_path(picker.handle_key(&enter)),
        Some(("insert-path".to_owned(), "/home/f29".to_owned())),
        "the trailing note is never highlighted"
    );
    assert_eq!(
        committed_path(picker.handle_key(&crate::test_util::key_event(
            PhysicalKey::P,
            ModSet::CTRL,
            None
        ))),
        None
    );
    assert_eq!(
        committed_path(picker.handle_key(&enter)),
        Some(("insert-path".to_owned(), "/home/f28".to_owned()))
    );
    assert_eq!(
        picker.handle_key(&press_text(PhysicalKey::Escape, None)),
        OverlayCommand::Dismiss
    );
}

/// The highlighted row stays on screen as it walks down a long list.
#[test]
fn the_view_follows_the_highlight() {
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    let area = Rect::new(0, 0, 100, 30);
    painted(&picker, area);
    browse_home(&mut picker);
    for _ in 0..25 {
        picker.handle_key(&press_text(PhysicalKey::ArrowDown, None));
    }
    let text = painted(&picker, area).join("\n");
    assert!(text.contains("f23"), "{text}");
    assert!(!text.contains("f00"), "scrolled past the top:\n{text}");
}

/// A click on a row commits it; a click on a note does nothing.
#[test]
fn clicking_a_row_commits_it() {
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    picker.update_paths(&Ok(PathResults {
        root: "/home".into(),
        parent: None,
        rows: ["f01", "f02"]
            .map(|name| PathRow {
                path: format!("/home/{name}"),
                kind: PathKind::File,
            })
            .to_vec(),
        status: PathStatus::Truncated,
    }));
    let area = Rect::new(0, 0, 100, 40);
    let rows = painted(&picker, area);
    let at = |needle: &str| {
        (0u16..)
            .zip(&rows)
            .find_map(|(y, row)| {
                let x = row[..row.find(needle)?].chars().count();
                Some((u16::try_from(x).expect("fits"), y))
            })
            .expect("painted")
    };
    let click = |picker: &mut PathPicker, (x, y): (u16, u16)| {
        let ev = |action| MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        };
        picker.handle_mouse(&ev(MouseAction::Press));
        picker.handle_mouse(&ev(MouseAction::Release))
    };
    let note = at("Results truncated");
    assert_eq!(click(&mut picker, note), OverlayCommand::Stay);
    let row = at("f02");
    assert_eq!(
        committed_path(click(&mut picker, row)),
        Some(("insert-path".to_owned(), "/home/f02".to_owned()))
    );
}

#[test]
fn bounds_cover_the_painted_dialog_and_never_shrink() {
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    let area = Rect::new(0, 0, 100, 40);
    browse_home(&mut picker);
    let rows = painted(&picker, area);
    let bounds = picker.bounds(area).expect("a modal");
    for (y, row) in (0u16..).zip(&rows) {
        for (x, ch) in (0u16..).zip(row.chars()) {
            let at = ratatui::layout::Position::new(x, y);
            assert!(
                ch == ' ' || bounds.contains(at),
                "{at:?} outside {bounds:?}"
            );
        }
    }
    picker.handle_key(&press_text(PhysicalKey::X, Some("x")));
    painted(&picker, area);
    assert_eq!(
        picker.bounds(area),
        Some(bounds),
        "a one-row refresh keeps the frame"
    );
}

/// Home and End jump the highlight to the first and last row, as in every
/// other picker.
#[test]
fn home_and_end_jump_the_highlight() {
    let enter = || press_text(PhysicalKey::Enter, None);
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    browse_home(&mut picker);
    picker.handle_key(&press_text(PhysicalKey::End, None));
    let last = committed_path(picker.handle_key(&enter()));
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    browse_home(&mut picker);
    picker.handle_key(&press_text(PhysicalKey::ArrowDown, None));
    picker.handle_key(&press_text(PhysicalKey::Home, None));
    let first = committed_path(picker.handle_key(&enter()));
    assert!(
        last.as_ref().is_some_and(|(_, path)| path.ends_with("f29")),
        "{last:?}"
    );
    assert_ne!(first, last);
}

/// A key after a layout change re-declares the picker offscreen at the
/// real viewport; the query prefix must not write past the scratch buffer.
#[test]
fn a_key_after_a_layout_change_on_a_large_viewport_does_not_panic() {
    let area = Rect::new(0, 0, 160, 60);
    let mut picker = PathPicker::new("/home".into(), &Theme::default());
    painted(&picker, area);
    picker.on_viewport_resize(160, 58);
    assert_eq!(
        picker.handle_key(&press_text(PhysicalKey::A, Some("a"))),
        OverlayCommand::Stay
    );
}

/// A viewport too short for the query row paints without panicking.
#[test]
fn a_tiny_viewport_paints_without_panicking() {
    for (w, h) in [(80, 3), (80, 1), (10, 3), (1, 1)] {
        for y in 0..h {
            let picker = PathPicker::new("/home".into(), &Theme::default());
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            picker.render(Rect::new(0, y, w, h - y), &mut buf);
        }
    }
}
