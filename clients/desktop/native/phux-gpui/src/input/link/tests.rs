use super::{
    Identity, LINK_REFUSED, LinkAdmit, PaintedLinks, admit_path, admit_url, allowed, link_in,
    link_span, url_around,
};
use phux_client_core::grid::{Cell, CellMetadata, GridBuffer, GridDamage};
use phux_client_runtime::publication::{Cursor, FrameColors, GridFrame, Scrollbar};
use std::sync::Arc;

#[test]
fn finds_the_url_under_the_offset_and_drops_trailing_punctuation() {
    let text = "see https://phux.dev/docs, then (http://a.b/c) done";
    assert_eq!(
        url_around(text, 10).as_deref(),
        Some("https://phux.dev/docs")
    );
    assert_eq!(url_around(text, 36).as_deref(), Some("http://a.b/c"));
    assert_eq!(url_around(text, 2), None);
    assert_eq!(url_around(text, text.len() - 1), None);
}

#[test]
fn keeps_balanced_parentheses_and_rejects_bare_schemes() {
    let text = "https://en.wikipedia.org/wiki/Rust_(language) x";
    assert_eq!(
        url_around(text, 5).as_deref(),
        Some("https://en.wikipedia.org/wiki/Rust_(language)")
    );
    assert_eq!(url_around("https:// nothing", 3), None);
}

#[test]
fn a_link_spans_exactly_the_cells_command_would_open() {
    let text = text_frame(&["see https://phux.dev/docs, now"], 1, 0);
    assert_eq!(
        link_span(&text, 0, 10),
        Some((4, 25)),
        "the URL, not its comma"
    );
    assert_eq!(link_span(&text, 0, 1), None, "prose is not a link");
    let osc8 = osc8_frame("https://phux.dev", 1);
    assert_eq!(
        link_span(&osc8, 0, 0),
        Some((0, 1)),
        "only the linked cells"
    );
    assert_eq!(link_span(&osc8, 0, 1), None);
}

#[test]
fn osc8_targets_are_limited_to_expected_schemes() {
    assert!(allowed("https://example.com"));
    assert!(allowed("file:///tmp/a"));
    assert!(!allowed("x-apple-systempreferences:com.apple"));
    assert!(!allowed("javascript:alert(1)"));
}

#[test]
fn an_unpainted_url_never_opens_instead_of_the_pressed_link() {
    let painted = text_frame(&["https://a.example"], 1, 0);
    let unseen = text_frame(&["https://b.example"], 9, 0);
    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    assert_eq!(link_in(&unseen, 0, 4).as_deref(), Some("https://b.example"));
    assert_eq!(
        links.release(0, 4).as_deref(),
        Some("https://a.example"),
        "the gesture has no path to a publication that was not painted"
    );
}

#[test]
fn an_osc8_replacement_under_the_cursor_does_not_open() {
    let pressed = osc8_frame("https://a.example", 1);
    let replaced = osc8_frame("https://b.example", 2);
    let mut links = hold(&pressed);
    assert!(links.press(0, 0));
    links.present(Identity::of(1, &replaced), replaced.clone());
    assert_eq!(
        link_in(&replaced, 0, 0).as_deref(),
        Some("https://b.example")
    );
    assert_eq!(links.release(0, 0), None);
}

#[test]
fn unrelated_output_keeps_the_pressed_link() {
    let painted = text_frame(&["https://a.example | ", "                    "], 1, 0);
    let spun = text_frame(&["https://a.example / ", "https://b.example   "], 2, 0);
    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    links.present(Identity::of(1, &spun), spun.clone());
    assert_eq!(link_in(&spun, 1, 0).as_deref(), Some("https://b.example"));
    assert_eq!(links.release(0, 10).as_deref(), Some("https://a.example"));
}

#[test]
fn dragging_off_the_link_cancels_it() {
    let painted = text_frame(&["https://a.example       "], 1, 0);
    let mut links = hold(&painted);
    assert!(links.press(0, 2));
    assert_eq!(
        links.release(0, 8).as_deref(),
        Some("https://a.example"),
        "a release still on the link completes the click"
    );
    let mut links = hold(&painted);
    assert!(links.press(0, 2));
    assert_eq!(links.release(0, 20), None);
}

#[test]
fn a_viewport_change_cancels_the_pressed_link() {
    let top = text_frame(&["https://a.example"], 1, 0);
    let scrolled = text_frame(&["https://a.example"], 2, 4);
    let mut links = hold(&top);
    assert!(links.press(0, 4));
    links.present(Identity::of(1, &scrolled), scrolled);
    assert_eq!(links.release(0, 4), None);

    let resized = text_frame(&["https://a.example!!"], 3, 0);
    let mut links = hold(&top);
    assert!(links.press(0, 4));
    links.present(Identity::of(1, &resized), resized);
    assert_eq!(links.release(0, 4), None);
}

#[test]
fn reconnect_or_forget_cancels_the_pressed_link() {
    let painted = text_frame(&["https://a.example"], 1, 0);
    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    let replaced = text_frame_at(&["https://a.example"], 9, 0, 4);
    links.present(Identity::of(1, &replaced), replaced);
    assert_eq!(links.release(0, 4), None);

    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    links.present(Identity::of(2, &painted), Arc::clone(&painted));
    assert_eq!(links.release(0, 4), None);

    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    links.cancel();
    assert_eq!(links.release(0, 4), None);

    let mut links = hold(&painted);
    assert!(links.press(0, 4));
    links.forget();
    assert_eq!(links.release(0, 4), None);
    assert!(!links.press(0, 4), "a forgotten frame has no link");
}

#[test]
fn rejected_schemes_are_not_a_press_and_file_links_still_open() {
    let script = osc8_frame("javascript:alert(1)", 1);
    let mut links = hold(&script);
    assert!(!links.press(0, 0));
    assert_eq!(links.release(0, 0), None);

    let file = text_frame(&["file:///tmp/a"], 1, 0);
    let mut links = hold(&file);
    assert!(links.press(0, 1));
    assert_eq!(links.release(0, 6).as_deref(), Some("file:///tmp/a"));
}

#[test]
fn web_links_open_and_file_links_follow_the_host() {
    let remote = crate::attach::TerminalHost::Remote {
        label: Some("mini".to_owned()),
    };
    assert!(matches!(
        admit_url("https://phux.dev/docs", &remote),
        LinkAdmit::Open
    ));
    assert!(matches!(
        admit_url("file:///tmp/notes.txt", &remote),
        LinkAdmit::Refuse {
            title: LINK_REFUSED,
            ..
        }
    ));
    match admit_url(
        "file://mini.local/tmp/notes.txt",
        &crate::attach::TerminalHost::Local,
    ) {
        LinkAdmit::Refuse { body, .. } => assert!(body.contains("another machine"), "{body}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(matches!(
        admit_url(
            "file://localhost/tmp/phux-link-does-not-exist.txt",
            &crate::attach::TerminalHost::Local
        ),
        LinkAdmit::Open
    ));
    assert!(matches!(
        admit_url(
            "file:///Applications/Safari.app",
            &crate::attach::TerminalHost::Local
        ),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(
            "file:///Applications/Safari.app/../notes.txt",
            &crate::attach::TerminalHost::Local
        ),
        LinkAdmit::Open
    ));
    assert!(matches!(
        admit_url("file:///tmp/Foo%2Eapp", &crate::attach::TerminalHost::Local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(
            "file:////server/share/a.txt",
            &crate::attach::TerminalHost::Local
        ),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url("javascript:alert(1)", &crate::attach::TerminalHost::Local),
        LinkAdmit::Refuse { .. }
    ));
}

#[cfg(unix)]
#[test]
fn programs_bundles_and_automounts_are_not_opened() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = std::env::temp_dir().join(format!("phux-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("notes.txt");
    std::fs::write(&doc, b"hi").unwrap();
    let program = dir.join("run");
    std::fs::write(&program, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bundle = dir.join("Tool.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();
    let alias = dir.join("alias.txt");
    symlink(&bundle, &alias).unwrap();
    let share = dir.join("share");
    symlink("/net/elsewhere/share/notes.txt", &share).unwrap();
    let looped = dir.join("loop");
    symlink("loop", &looped).unwrap();
    let command = dir.join("build.command");
    std::fs::write(&command, b"echo hi\n").unwrap();

    let local = crate::attach::TerminalHost::Local;
    assert!(matches!(
        admit_url(&file_url_of(&doc), &local),
        LinkAdmit::Open
    ));
    assert!(matches!(admit_path(&doc), LinkAdmit::Open));
    assert!(matches!(
        admit_url(&file_url_of(&program), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(admit_path(&program), LinkAdmit::Refuse { .. }));
    assert!(matches!(
        admit_url(&file_url_of(&bundle), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(&file_url_of(&alias), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(&file_url_of(&share), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(&file_url_of(&looped), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url(&file_url_of(&command), &local),
        LinkAdmit::Refuse { .. }
    ));
    assert!(matches!(
        admit_url("file:///net/other/share/notes.txt", &local),
        LinkAdmit::Refuse { .. }
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn file_url_of(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

fn hold(frame: &Arc<GridFrame>) -> PaintedLinks {
    let mut links = PaintedLinks::default();
    links.present(Identity::of(1, frame), Arc::clone(frame));
    links
}

fn text_frame(lines: &[&str], seq: u64, offset: u64) -> Arc<GridFrame> {
    text_frame_at(lines, seq, offset, 3)
}

fn text_frame_at(lines: &[&str], seq: u64, offset: u64, bootstrap: u64) -> Arc<GridFrame> {
    let cols = u16::try_from(lines.iter().map(|line| line.len()).max().unwrap_or(0)).unwrap();
    let rows = u16::try_from(lines.len()).unwrap();
    let mut utf8 = Vec::new();
    let mut cells = Vec::new();
    for line in lines {
        let bytes = line.as_bytes();
        for col in 0..cols {
            let byte = bytes.get(usize::from(col)).copied().unwrap_or(b' ');
            let start = u32::try_from(utf8.len()).unwrap();
            utf8.push(byte);
            cells.push(Cell {
                utf8_offset: start,
                utf8_len: 1,
                ..Cell::default()
            });
        }
    }
    Arc::new(grid(cols, rows, seq, bootstrap, cells, utf8, offset))
}

fn osc8_frame(uri: &str, seq: u64) -> Arc<GridFrame> {
    let mut utf8 = b"docs".to_vec();
    let link_at = u32::try_from(utf8.len()).unwrap();
    utf8.extend_from_slice(uri.as_bytes());
    let linked = Cell {
        utf8_offset: 0,
        utf8_len: 4,
        hyperlink_offset: link_at,
        hyperlink_len: u32::try_from(uri.len()).unwrap(),
        ..Cell::default()
    };
    Arc::new(grid(2, 1, seq, 3, vec![linked, Cell::default()], utf8, 0))
}

fn grid(
    cols: u16,
    rows: u16,
    seq: u64,
    bootstrap: u64,
    cells: Vec<Cell>,
    utf8: Vec<u8>,
    offset: u64,
) -> GridFrame {
    GridFrame {
        terminal_id: phux_protocol::ResourceId::local(1),
        generation: seq,
        stream_id: 7,
        bootstrap_id: bootstrap,
        last_seq: seq,
        cols,
        rows,
        cursor: Cursor::default(),
        scrollbar: Scrollbar {
            total: 40,
            offset,
            len: u64::from(rows),
        },
        colors: FrameColors::default(),
        damage: GridDamage::Full,
        images: Vec::new(),
        buffer: GridBuffer {
            cells,
            utf8,
            metadata: vec![CellMetadata::default(); usize::from(cols) * usize::from(rows)],
            row_dirty: vec![true; usize::from(rows)],
            ..GridBuffer::default()
        },
    }
}
