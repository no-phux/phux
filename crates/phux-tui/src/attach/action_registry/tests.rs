//! Registry tests: the table matches the config vocabulary, and the palette
//! renders from it.

use super::*;
use std::collections::BTreeSet;

/// The glyphs of a VT stream, CSI escapes dropped.
fn strip_csi(vt: &str) -> String {
    let mut out = String::new();
    let mut chars = vt.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // Skip the `[` and the parameter bytes through the final byte
        // (0x40-0x7E). Only CSI sequences are emitted here.
        for esc in chars.by_ref() {
            if esc != '[' && ('@'..='~').contains(&esc) {
                break;
            }
        }
    }
    out
}

/// The real palette overflows its modal, so rows past the fold must be
/// reachable by scrolling (they once painted off the box's bottom edge).
#[test]
fn the_real_palette_scrolls_to_its_last_row() {
    use crate::render::Theme;
    use crate::render::overlay::{OverlayState, SelectList};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    let items = palette_items(None, &[], &[]);
    let last = items
        .iter()
        .rev()
        .find(|i| !i.is_header())
        .expect("the registry has at least one action")
        .label
        .clone();

    let mut overlays = OverlayState::new();
    overlays.push(Box::new(SelectList::new(
        "command palette",
        items,
        &Theme::default(),
    )));
    let paint = |overlays: &OverlayState| {
        let mut out = Vec::new();
        overlays.paint(&mut out, (160, 48)).expect("paint");
        strip_csi(&String::from_utf8_lossy(&out))
    };

    let opened = paint(&overlays);
    assert!(
        !opened.contains(&last),
        "the real palette must overflow its modal for this test to prove \
         anything — `{last}` was expected below the fold",
    );
    assert!(
        opened.contains('┃'),
        "an overflowing palette must paint a scrollbar so the user can see \
         there is more list:\n{opened:?}",
    );

    // End jumps to the last row: it must now be painted inside the box.
    overlays.handle_key(&KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::End,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    });
    let scrolled = paint(&overlays);
    assert!(
        scrolled.contains(&last),
        "the last action `{last}` must be reachable, not clipped away",
    );
}

/// [`ACTIONS`] declares every `ACTION_NAMES` entry exactly once and
/// nothing else, so config validation and dispatch agree.
#[test]
fn the_registry_declares_every_action_name_exactly_once() {
    let names: BTreeSet<&str> = phux_config::vocab::ACTION_NAMES.iter().copied().collect();
    assert_eq!(
        names.len(),
        phux_config::vocab::ACTION_NAMES.len(),
        "ACTION_NAMES repeats a name",
    );
    let mut declared = BTreeSet::new();
    for spec in ACTIONS {
        assert!(
            declared.insert(spec.name),
            "`{}` is declared twice",
            spec.name
        );
        assert!(
            names.contains(spec.name),
            "`{}` is declared but missing from phux_config::vocab::ACTION_NAMES, \
             so config validation would reject a binding to it",
            spec.name,
        );
    }
    for name in &names {
        assert!(
            declared.contains(name),
            "`{name}` is in ACTION_NAMES but has no ActionSpec in ACTIONS",
        );
    }
}

/// Every row documents itself, and a hidden one says why.
#[test]
fn every_action_is_documented() {
    for spec in ACTIONS {
        assert!(
            !spec.description.trim().is_empty(),
            "`{}` has an empty description",
            spec.name
        );
        if let Palette::Hidden { reason } = spec.palette {
            assert!(
                !reason.trim().is_empty(),
                "`{}` has an empty reason",
                spec.name
            );
        }
    }
}

#[test]
fn signal_terminal_palette_default_is_the_reversible_freeze() {
    // ADR-0033: signals are not lease-gated server-side, so the palette
    // default must stay the reversible `freeze`.
    let sig = find("signal-terminal").expect("signal-terminal registered");
    assert_eq!(
        sig.resolved_action().args.get("signal"),
        Some(&toml::Value::String("freeze".to_owned())),
        "the palette default signal must remain the reversible freeze",
    );
}

/// The palette offers both split directions, each committing the
/// `direction` its title says, so `spl` finds "Split below" as well as
/// "Split right".
#[test]
fn palette_lists_split_below_beside_split_right() {
    let items = palette_items(None, &[], &[]);
    let direction_of = |title: &str| {
        items
            .iter()
            .find(|i| i.label == title)
            .unwrap_or_else(|| panic!("{title} row"))
            .action
            .args
            .get("direction")
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned)
    };
    assert_eq!(direction_of("Split right").as_deref(), Some("vertical"));
    assert_eq!(direction_of("Split below").as_deref(), Some("horizontal"));
    let pane_rows: Vec<&str> = items
        .iter()
        .take_while(|i| i.label != "Tab")
        .filter(|i| !i.is_header())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(&pane_rows[..2], ["Split right", "Split below"]);
}

#[test]
fn palette_items_show_no_chord_when_no_config() {
    let items = palette_items(None, &[], &[]);
    assert!(
        items
            .iter()
            .filter(|i| !i.is_header())
            .all(|i| i.secondary.is_none()),
        "no config ⇒ no row claims a chord",
    );
}

#[test]
fn palette_items_group_under_category_headers() {
    let items = palette_items(None, &[], &[]);
    // Every category with members contributes exactly one header, in
    // ORDER, each immediately followed by indented action rows.
    let headers: Vec<&str> = items
        .iter()
        .filter(|i| i.is_header())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux"]);

    // Selectable rows are indented (nested under their header); headers
    // are not.
    for item in &items {
        if item.is_header() {
            assert!(!item.indented, "header `{}` must not indent", item.label);
        } else {
            assert!(item.indented, "row `{}` must indent", item.label);
        }
    }

    // The first row is a header (Pane), not a bare action.
    assert!(items[0].is_header(), "palette opens with a category header");
}

// ---------- phux-r82.5: dynamic plugin rows ----------

fn plugin_entry(keys: Option<&str>) -> super::super::plugin_actions::PluginActionEntry {
    super::super::plugin_actions::PluginActionEntry {
        plugin_id: "com.example.tools".to_owned(),
        plugin_name: "Agent Tools".to_owned(),
        action_id: "summarize".to_owned(),
        title: "Summarize pane".to_owned(),
        keys: keys.map(ToOwned::to_owned),
    }
}

#[test]
fn plugin_actions_inject_namespaced_rows_under_plugin_header() {
    let items = palette_items(None, &[plugin_entry(None)], &[]);
    // The static categories are unchanged and the Plugin header trails.
    let headers: Vec<&str> = items
        .iter()
        .filter(|i| i.is_header())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux", "Plugin"]);

    let row = items
        .iter()
        .find(|i| !i.is_header() && i.label.starts_with("plugin: "))
        .expect("plugin row present");
    assert_eq!(row.label, "plugin: Agent Tools: Summarize pane");
    assert!(row.indented, "plugin rows nest under their header");
    // The committed action is the shared dispatcher action with the
    // plugin/id args — same shape a merged keybinding produces.
    assert_eq!(row.action.action, "plugin-action");
    assert_eq!(
        row.action.args.get("plugin"),
        Some(&toml::Value::String("com.example.tools".to_owned()))
    );
    assert_eq!(
        row.action.args.get("id"),
        Some(&toml::Value::String("summarize".to_owned()))
    );
}

#[test]
fn plugin_row_shows_merged_binding_chord() {
    let entry = plugin_entry(Some("g"));
    let mut kb = KeybindingsCfg::default();
    super::super::plugin_actions::merge_plugin_bindings(&mut kb, std::slice::from_ref(&entry));
    let items = palette_items(Some(&kb), &[entry], &[]);
    let row = items
        .iter()
        .find(|i| i.label.starts_with("plugin: "))
        .expect("plugin row present");
    assert_eq!(row.secondary.as_deref(), Some("C-a g"));
}

// ---------- phux-r82.7: dynamic plugin pane rows ----------

fn pane_entry() -> PluginPaneEntry {
    PluginPaneEntry {
        plugin_id: "com.example.tools".to_owned(),
        plugin_name: "Agent Tools".to_owned(),
        pane_id: "board".to_owned(),
        title: "Agent Board".to_owned(),
        placement: super::super::plugin_panes::HostedPlacement::Split,
        command: vec!["agent-board".to_owned()],
        plugin_root: std::path::PathBuf::from("/x"),
    }
}

#[test]
fn plugin_panes_inject_namespaced_rows_under_shared_plugin_header() {
    let items = palette_items(None, &[plugin_entry(None)], &[pane_entry()]);
    // One shared Plugin header for actions and panes together.
    let headers: Vec<&str> = items
        .iter()
        .filter(|i| i.is_header())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(headers, vec!["Pane", "Tab", "Session", "phux", "Plugin"]);

    let row = items
        .iter()
        .find(|i| !i.is_header() && i.label.starts_with("plugin pane: "))
        .expect("plugin pane row present");
    assert_eq!(row.label, "plugin pane: Agent Tools: Agent Board");
    assert!(row.indented, "plugin pane rows nest under their header");
    assert_eq!(row.action.action, "plugin-pane");
    assert_eq!(
        row.action.args.get("plugin"),
        Some(&toml::Value::String("com.example.tools".to_owned()))
    );
    assert_eq!(
        row.action.args.get("pane"),
        Some(&toml::Value::String("board".to_owned()))
    );
}
