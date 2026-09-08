//! Context-menu contents (phux-wrnm, ADR-0058).
//!
//! The right-click menus are three fixed row sets — one per thing you can
//! right-click: a pane, a window (its status-bar tab or sidebar row), and
//! the session chrome around them. This module owns *what is on* each
//! menu; [`crate::render::overlay::menu`] owns how one looks and behaves.
//!
//! Every row commits a [`ResolvedAction`], so choosing one runs exactly
//! what the equivalent keybinding, palette row, or sidebar click runs —
//! the single `run_action` dispatch path, no bespoke click semantics
//! (the invariant [`super::action_registry`] documents). Rows are
//! annotated with their currently-bound chord when there is one, so the
//! menu doubles as a discovery surface for the keyboard equivalents.
//!
//! Every action named here must be dispatched by `run_action`; the unit
//! test `menu_actions_are_dispatched_names` holds that in lockstep the
//! same way the sidebar and palette tests do.

use std::collections::BTreeMap;

use phux_config::KeybindingsCfg;
use phux_config::keybind::ResolvedAction;

use crate::render::overlay::MenuRow;

/// A menu's border title and its rows.
pub(super) struct MenuSpec {
    /// Border title — the target the menu acts on (`"pane"`, a window
    /// name, a session name).
    pub title: String,
    /// Rows, top to bottom.
    pub rows: Vec<MenuRow>,
}

/// The menu for a pane: the splits, the view toggles, and the close.
///
/// `zoomed` flips the zoom row's label so it names what the click will do
/// rather than what is currently true.
pub(super) fn pane_menu(keybindings: Option<&KeybindingsCfg>, zoomed: bool) -> MenuSpec {
    let rows = vec![
        row(
            keybindings,
            "Split right",
            "split-pane",
            &[("direction", "vertical")],
        ),
        row(
            keybindings,
            "Split down",
            "split-pane",
            &[("direction", "horizontal")],
        ),
        row(
            keybindings,
            if zoomed { "Unzoom" } else { "Zoom" },
            "toggle-zoom",
            &[],
        ),
        row(keybindings, "Copy mode", "copy-mode", &[]),
        MenuRow::Separator,
        row(keybindings, "All commands…", "command-palette", &[]),
        MenuRow::Separator,
        row(keybindings, "Close pane", "kill-pane", &[]),
    ];
    MenuSpec {
        title: "pane".to_owned(),
        rows,
    }
}

/// The menu for a window. The caller has already made `name`'s window
/// active (a right-click on a tab selects it first, exactly as a left
/// click does), so the rows act on the active window with no target arg.
pub(super) fn window_menu(keybindings: Option<&KeybindingsCfg>, name: &str) -> MenuSpec {
    let rows = vec![
        row(keybindings, "New window", "new-window", &[]),
        row(keybindings, "Rename window…", "rename-window", &[]),
        row(keybindings, "Pick window…", "window-picker", &[]),
        MenuRow::Separator,
        row(keybindings, "All commands…", "command-palette", &[]),
        MenuRow::Separator,
        row(keybindings, "Close window", "kill-window", &[]),
    ];
    MenuSpec {
        title: if name.is_empty() {
            "window".to_owned()
        } else {
            name.to_owned()
        },
        rows,
    }
}

/// The menu for the session chrome — the sidebar's blank rows, the
/// status bar outside the tabs. The broad "what can I do here" menu.
pub(super) fn session_menu(keybindings: Option<&KeybindingsCfg>, session: &str) -> MenuSpec {
    let rows = vec![
        row(keybindings, "New window", "new-window", &[]),
        row(keybindings, "Pick window…", "window-picker", &[]),
        row(keybindings, "Pick session…", "session-picker", &[]),
        row(keybindings, "Rename session…", "rename-session", &[]),
        MenuRow::Separator,
        row(keybindings, "Agent fleet", "agent-fleet", &[]),
        row(keybindings, "Toggle sidebar", "toggle-sidebar", &[]),
        row(keybindings, "All commands…", "command-palette", &[]),
        MenuRow::Separator,
        row(keybindings, "Detach", "detach", &[]),
    ];
    MenuSpec {
        title: if session.is_empty() {
            "session".to_owned()
        } else {
            session.to_owned()
        },
        rows,
    }
}

/// One menu row: `label` committing `action` with inline string `args`,
/// annotated with the chord bound to that exact action when there is one.
fn row(
    keybindings: Option<&KeybindingsCfg>,
    label: &str,
    action: &str,
    args: &[(&str, &str)],
) -> MenuRow {
    let resolved = ResolvedAction {
        action: action.to_owned(),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_owned(), toml::Value::String((*v).to_owned())))
            .collect::<BTreeMap<_, _>>(),
    };
    let chord = super::action_registry::bound_chord_for(keybindings, &resolved);
    MenuRow::item(label, resolved).secondary(chord)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::attach::input_dispatch::ACTION_NAMES;

    fn actions(spec: &MenuSpec) -> Vec<String> {
        spec.rows
            .iter()
            .filter_map(|row| match row {
                MenuRow::Item { action, .. } => Some(action.action.clone()),
                MenuRow::Separator => None,
            })
            .collect()
    }

    /// The same lockstep the palette registry and sidebar click tests
    /// enforce: a menu row that names an action `run_action` does not
    /// dispatch is a dead row.
    #[test]
    fn menu_actions_are_dispatched_names() {
        let specs = [
            pane_menu(None, false),
            window_menu(None, "editor"),
            session_menu(None, "work"),
        ];
        for spec in &specs {
            for action in actions(spec) {
                assert!(
                    ACTION_NAMES.contains(&action.as_str()),
                    "menu `{}` offers `{action}`, which run_action does not dispatch",
                    spec.title,
                );
            }
        }
    }

    #[test]
    fn every_menu_opens_on_a_selectable_row() {
        for spec in [
            pane_menu(None, false),
            window_menu(None, "editor"),
            session_menu(None, "work"),
        ] {
            assert!(
                spec.rows.first().is_some_and(MenuRow::is_selectable),
                "menu `{}` must not open on a separator",
                spec.title,
            );
        }
    }

    #[test]
    fn the_zoom_row_names_what_the_click_will_do() {
        let unzoomed = pane_menu(None, false);
        let zoomed = pane_menu(None, true);
        let label = |spec: &MenuSpec| {
            spec.rows
                .iter()
                .find_map(|row| match row {
                    MenuRow::Item { label, action, .. } if action.action == "toggle-zoom" => {
                        Some(label.clone())
                    }
                    _ => None,
                })
                .expect("the pane menu has a zoom row")
        };
        assert_eq!(label(&unzoomed), "Zoom");
        assert_eq!(label(&zoomed), "Unzoom");
    }

    #[test]
    fn split_rows_carry_the_direction_the_label_promises() {
        let spec = pane_menu(None, false);
        let dir = |label: &str| {
            spec.rows
                .iter()
                .find_map(|row| match row {
                    MenuRow::Item {
                        label: l, action, ..
                    } if l == label => action.args.get("direction").and_then(toml::Value::as_str),
                    _ => None,
                })
                .map(ToOwned::to_owned)
                .expect("split row present")
        };
        // `direction` names the divider, not the split axis: a vertical
        // divider puts the new pane on the right.
        assert_eq!(dir("Split right"), "vertical");
        assert_eq!(dir("Split down"), "horizontal");
    }

    #[test]
    fn rows_are_annotated_with_their_bound_chord() {
        // `|` under the leader, carrying the same args the menu row does,
        // is the binding the shipped config template uses.
        let mut kb = KeybindingsCfg::default();
        kb.prefix_table.insert(
            "|".to_owned(),
            phux_config::Action::Parameterized(phux_config::ParamAction {
                action: "split-pane".to_owned(),
                args: std::iter::once((
                    "direction".to_owned(),
                    toml::Value::String("vertical".into()),
                ))
                .collect(),
            }),
        );
        let spec = pane_menu(Some(&kb), false);
        let split = spec
            .rows
            .iter()
            .find_map(|row| match row {
                MenuRow::Item {
                    label, secondary, ..
                } if label == "Split right" => Some(secondary.clone()),
                _ => None,
            })
            .expect("split row present");
        assert_eq!(
            split.as_deref(),
            Some("C-a |"),
            "the row shows the literal keystrokes that run it",
        );
        // With no config nothing is annotated — an unbound row shows no
        // chord rather than the word "unbound" (a menu is not a table).
        let bare = pane_menu(None, false);
        assert!(bare.rows.iter().all(|row| matches!(
            row,
            MenuRow::Separator
                | MenuRow::Item {
                    secondary: None,
                    ..
                }
        )));
    }

    #[test]
    fn a_nameless_target_still_gets_a_title() {
        assert_eq!(window_menu(None, "").title, "window");
        assert_eq!(session_menu(None, "").title, "session");
    }
}
