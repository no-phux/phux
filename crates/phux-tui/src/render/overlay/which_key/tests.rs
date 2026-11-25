//! Which-key legend and grid tests.

use super::legend::*;
use super::*;
use phux_config::{Action, ParamAction};
use std::collections::BTreeMap;

fn cfg_with(prefix: &str, entries: &[(&str, &str)]) -> KeybindingsCfg {
    let prefix_table: BTreeMap<String, Action> = entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Action::Bare((*v).to_owned())))
        .collect();
    KeybindingsCfg {
        prefix: prefix.to_owned(),
        prefix_table,
        ..KeybindingsCfg::default()
    }
}

/// `(keys, label)` per row, in display order.
fn pairs(overlay: &WhichKeyOverlay) -> Vec<(String, String)> {
    overlay
        .rows
        .iter()
        .map(|row| (row.key_text(), row.label.clone()))
        .collect()
}

fn render_to_string(overlay: &WhichKeyOverlay, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    (0..height)
        .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
        .collect()
}

#[test]
fn rows_reflect_rebound_keys_not_a_hardcoded_table() {
    let overlay = WhichKeyOverlay::from_config(
        &cfg_with("C-a", &[("q", "detach"), ("v", "copy-mode")]),
        &Theme::default(),
    );
    assert!(pairs(&overlay).contains(&("q".to_owned(), "Detach".to_owned())));
    assert!(
        !pairs(&overlay).iter().any(|(k, _)| k == "d"),
        "unbound default keys must not appear"
    );
    let text = render_to_string(&overlay, 120, 30);
    assert!(text.contains('q') && text.contains("Detach"), "{text}");

    // A rebound prefix shows in the title.
    let overlay =
        WhichKeyOverlay::from_config(&cfg_with("C-Space", &[("d", "detach")]), &Theme::default());
    assert!(render_to_string(&overlay, 80, 24).contains("C-Space"));
}

#[test]
fn parameterized_actions_label_with_args() {
    let mut cfg = cfg_with("C-a", &[]);
    cfg.prefix_table.insert(
        "%".to_owned(),
        param(
            "split-pane",
            &[("direction", toml::Value::String("vertical".to_owned()))],
        ),
    );
    let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
    assert!(
        pairs(&overlay).contains(&("%".to_owned(), "Split right".to_owned())),
        "rows: {:?}",
        overlay.rows
    );
}

#[test]
fn numeric_tab_jumps_collapse_to_one_row() {
    let mut cfg = cfg_with("C-a", &[("d", "detach")]);
    for i in 0..10u8 {
        cfg.prefix_table.insert(
            i.to_string(),
            param(
                "select-window",
                &[("index", toml::Value::Integer(i.into()))],
            ),
        );
    }
    let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
    let rows = pairs(&overlay);
    let collapsed = rows.iter().filter(|(_, a)| a == "Go to tab").count();
    assert_eq!(collapsed, 1);
    assert!(rows.iter().any(|(k, _)| k == "0-9"));
    assert!(!rows.iter().any(|(k, _)| k == "0" || k == "9"));
}

#[test]
fn popup_is_passthrough_and_bounded() {
    use phux_protocol::input::key::PhysicalKey;
    let mut overlay =
        WhichKeyOverlay::from_config(&cfg_with("C-a", &[("d", "detach")]), &Theme::default());
    assert!(overlay.is_input_passthrough());
    let area = Rect::new(0, 0, 80, 24);
    assert!(overlay.bounds(area).is_some(), "floats over the panes");
    // Defensive handle_key dismisses (never consumes).
    let key = crate::test_util::press(PhysicalKey::A);
    assert_eq!(overlay.handle_key(&key), OverlayCommand::Dismiss);
}

#[test]
fn empty_prefix_table_shows_notice() {
    let overlay = WhichKeyOverlay::from_config(&cfg_with("C-a", &[]), &Theme::default());
    let text = render_to_string(&overlay, 120, 30);
    assert!(
        text.contains("No prefix bindings configured."),
        "empty notice:\n{text}"
    );
}

fn param(action: &str, args: &[(&str, toml::Value)]) -> Action {
    Action::Parameterized(ParamAction {
        action: action.to_owned(),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    })
}

/// Labels read as phrases a person says, not as call syntax, and a
/// binding the palette offers reads as its palette title.
#[test]
fn labels_read_as_phrases() {
    let s = |v: &str| toml::Value::String(v.to_owned());
    let int = toml::Value::Integer;
    let bare = |name: &str| Action::Bare(name.to_owned());
    let cases = [
        (bare("kill-pane"), "Kill pane…"),
        (bare("take-input"), "Take the wheel"),
        (bare("window-picker"), "Go to tab…"),
        (
            param(
                "resize-pane",
                &[("direction", s("left")), ("amount", int(5))],
            ),
            "Grow pane left",
        ),
        (param("move-window", &[("delta", int(-1))]), "Move tab left"),
        (
            param("signal-terminal", &[("signal", s("freeze"))]),
            "Freeze pane",
        ),
        (
            param("plugin-action", &[("id", s("summarize"))]),
            "Plugin action summarize",
        ),
    ];
    for (action, want) in cases {
        assert_eq!(action_label(&action), want, "{action:?}");
    }
}

/// On a roomy screen the grid shows every binding: nothing is cut off
/// the bottom of a single column.
#[test]
fn every_binding_fits_on_a_roomy_screen() {
    let entries: Vec<(String, String)> = (b'a'..=b'z')
        .chain(b'A'..=b'P')
        .map(|c| ((c as char).to_string(), format!("action-{}", c as char)))
        .collect();
    let refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let overlay = WhichKeyOverlay::from_config(&cfg_with("C-a", &refs), &Theme::default());
    let text = render_to_string(&overlay, 160, 40);
    for (_, action) in &entries {
        let label = sentence_case(&action.replace('-', " "));
        assert!(text.contains(&label), "{label} missing:\n{text}");
    }
}

/// The count in the overflow note, if the text carries one.
fn overflow_count(text: &str) -> Option<usize> {
    let rest = text.split("+ ").nth(1)?;
    let (count, tail) = rest.split_once(' ')?;
    tail.starts_with("more").then_some(())?;
    count.parse().ok()
}

fn shipped_overlay() -> WhichKeyOverlay {
    let cfg = phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("shipped config parses")
    .keybindings;
    WhichKeyOverlay::from_config(&cfg, &Theme::default())
}

/// Rows of the shipped table whose label is absent from `text`.
fn unrendered(overlay: &WhichKeyOverlay, area: Rect, text: &str) -> usize {
    let label_w = overlay.grid(area).label_w;
    overlay
        .rows
        .iter()
        .filter(|row| !text.contains(&crate::render::clip_text(&row.label, label_w)))
        .count()
}

/// A viewport too small for the shipped table still paints inside the
/// screen and counts the bindings it left out.
fn assert_short_viewport_says_what_it_left_out(width: u16, height: u16) {
    let overlay = shipped_overlay();
    let area = Rect::new(0, 0, width, height);
    let bounds = overlay.bounds(area).expect("bounds");
    assert!(
        bounds.height <= height && bounds.width <= width,
        "{width}x{height}: {bounds:?}"
    );
    let text = render_to_string(&overlay, width, height);
    let hidden = unrendered(&overlay, area, &text);
    assert!(
        hidden > 0,
        "{width}x{height} was meant to be too small:\n{text}"
    );
    assert_eq!(
        overflow_count(&text),
        Some(hidden),
        "{width}x{height}:\n{text}"
    );
    let last_column = overlay
        .grid(area)
        .columns
        .into_iter()
        .last()
        .expect("a column");
    assert!(
        matches!(last_column.last(), Some(Cell::Overflow(_))),
        "{width}x{height}: the note ends the last column"
    );
    assert!(
        matches!(
            last_column.get(last_column.len().wrapping_sub(2)),
            Some(Cell::Binding(_))
        ),
        "{width}x{height}: no header or gap above the note: {last_column:?}"
    );
}

#[test]
fn short_viewports_say_what_they_left_out() {
    for (width, height) in [(80, 24), (60, 20)] {
        assert_short_viewport_says_what_it_left_out(width, height);
    }
}

/// Two keys for one action are one row; a directional family is one row
/// with its keys in left/down/up/right order.
#[test]
fn duplicate_and_directional_bindings_share_a_row() {
    let s = |v: &str| toml::Value::String(v.to_owned());
    let mut cfg = cfg_with(
        "C-a",
        &[("Space", "command-palette"), (":", "command-palette")],
    );
    for (key, direction) in [("l", "right"), ("h", "left"), ("k", "up"), ("j", "down")] {
        cfg.prefix_table.insert(
            key.to_owned(),
            param("focus-direction", &[("direction", s(direction))]),
        );
    }
    let rows = pairs(&WhichKeyOverlay::from_config(&cfg, &Theme::default()));
    assert!(
        rows.contains(&(": Space".to_owned(), "Command palette".to_owned())),
        "{rows:?}"
    );
    assert!(
        rows.contains(&("h j k l".to_owned(), "Focus".to_owned())),
        "{rows:?}"
    );
    assert_eq!(rows.len(), 2, "{rows:?}");
}

/// Bindings group under Pane / Tab / Session / phux headers, in that
/// order, whatever order the keys sort in.
#[test]
fn bindings_render_under_group_headers() {
    let overlay = WhichKeyOverlay::from_config(
        &cfg_with(
            "C-a",
            &[
                ("s", "session-picker"),
                ("x", "kill-pane"),
                ("c", "new-window"),
                ("S", "settings"),
            ],
        ),
        &Theme::default(),
    );
    let groups: Vec<Group> = overlay.rows.iter().map(|r| r.group).collect();
    assert_eq!(
        groups,
        [Group::Pane, Group::Window, Group::Session, Group::Phux]
    );
    let text = render_to_string(&overlay, 160, 40);
    for header in ["Pane", "Tab", "Session", "phux"] {
        assert!(text.contains(header), "{header} missing:\n{text}");
    }
    assert!(
        text.contains(" esc ") && !text.contains("cancel"),
        "legend missing:\n{text}"
    );
}

/// The shipped prefix table keeps every group whole, each column opening
/// on a header, on a roomy screen; shows every binding on a modest one;
/// and stays inside a classic 80x24.
#[test]
fn shipped_table_fits_roomy_and_classic_screens() {
    let overlay = shipped_overlay();
    let roomy = overlay.grid(Rect::new(0, 0, 160, 45));
    let headers = (roomy.columns.iter().flatten())
        .filter(|cell| matches!(cell, Cell::Header(_)))
        .count();
    assert_eq!(headers, 4, "{roomy:?}");
    for (column, cells) in roomy.columns.iter().enumerate() {
        assert!(
            matches!(cells.first(), Some(Cell::Header(_))),
            "column {column}: {cells:?}"
        );
    }
    let classic = Rect::new(0, 0, 80, 24);
    let bounds = overlay.bounds(classic).expect("floats");
    assert!(bounds.height <= classic.height && bounds.width <= classic.width);
    let text = render_to_string(&overlay, 120, 30);
    for row in &overlay.rows {
        let label = crate::render::clip_text(&row.label, LABEL_MAX);
        assert!(text.contains(&label), "{label} missing:\n{text}");
    }
}

/// An 80x24 terminal leaves the popup a 61x23 pane area beside the
/// sidebar: too small for the table, so every group keeps its first rows
/// (Detach among them) and the note names the key that lists the rest.
#[test]
fn a_cramped_popup_keeps_every_group_and_points_at_the_palette() {
    let overlay = shipped_overlay();
    let text = render_to_string(&overlay, 61, 23);
    for header in ["Pane", "Tab", "Session", "phux"] {
        assert!(text.contains(header), "{header} missing:\n{text}");
    }
    assert!(text.contains("Detach"), "{text}");
    assert!(text.contains(": lists all"), "{text}");
}

/// A binding sits under the palette's section for its action, not a guess
/// from its name: `}` is a pane's next tab, `T` a new worktree's session.
#[test]
fn a_binding_takes_the_palette_section() {
    assert_eq!(Group::of("next-slot-tab"), Group::Pane);
    assert_eq!(Group::of("previous-slot-tab"), Group::Pane);
    assert_eq!(Group::of("new-worktree"), Group::Session);
    assert_eq!(Group::of("settings"), Group::Phux);
    // Detach is phux's own chrome in the palette, so here too.
    assert_eq!(Group::of("detach"), Group::Phux);
}
