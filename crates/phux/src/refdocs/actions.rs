//! The generated actions reference: every dispatcher action with its
//! parameters, description, and palette placement, in `ACTION_NAMES` order
//! from phux-tui's action registry `ACTIONS` (a phux-tui test pins the two
//! to the same names).

use phux_config::vocab::ACTION_NAMES;
use phux_tui::attach::action_registry::{ACTIONS, Palette, find};

use super::Page;

/// Escape `|` for use inside a markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Render `docs/reference/actions.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "An action is what a keybinding, palette row, context-menu entry, \
         or hook binds to: a bare name, or a name plus parameters \
         (`{ action = \"split-pane\", direction = \"vertical\" }` in \
         config TOML). The table lists every action the TUI dispatcher \
         handles, in the canonical `ACTION_NAMES` order; unit tests pin \
         this inventory to the dispatcher and the command palette, so an \
         action appears here exactly when the binary handles it.\n\n\
         The **Palette** column is the command-palette section the action \
         is offered under; a dash means the palette deliberately has no \
         row for it (reasons follow the table). An empty **Parameters** \
         cell means the action takes none.\n\n\
         | Action | Palette | Parameters | Description |\n\
         |---|---|---|---|\n",
    );
    // A name with no row is unreachable: the registry test in phux-tui
    // fails first.
    for spec in ACTION_NAMES.iter().filter_map(|name| find(name)) {
        let placement = spec
            .palette_row()
            .map_or("—", |(_, category)| category.header());
        let _ = writeln!(
            body,
            "| `{}` | {placement} | {} | {} |",
            spec.name,
            cell(spec.params),
            cell(spec.description),
        );
    }

    body.push_str("\nWhy the dash rows have no palette entry:\n\n");
    for spec in ACTIONS {
        if let Palette::Hidden { reason } = spec.palette {
            let _ = writeln!(body, "- `{}` — {reason}.", spec.name);
        }
    }

    Page {
        file: "actions.md",
        title: "phux actions reference",
        summary: "Every dispatcher action with its parameters, description, \
                  and palette placement.",
        lede: "Every action a key binding, palette row, context menu, or hook can dispatch, with its parameters.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{ACTION_NAMES, page};

    /// The page carries one table row per canonical action name — the
    /// page-level face of the registry test in phux-tui.
    #[test]
    fn actions_page_has_a_row_for_every_action_name() {
        let page = page();
        for &name in ACTION_NAMES {
            assert!(
                page.body.contains(&format!("| `{name}` |")),
                "generated actions reference has no row for `{name}`"
            );
        }
        let rows = page
            .body
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(
            rows,
            ACTION_NAMES.len(),
            "row count must match ACTION_NAMES exactly"
        );
    }

    /// Raw `|` inside params/description strings would silently shear a
    /// markdown table row into extra columns; the renderer must escape it.
    #[test]
    fn table_rows_keep_their_column_count() {
        let page = page();
        for line in page.body.lines().filter(|line| line.starts_with("| `")) {
            let columns = line.matches(" | ").count();
            assert_eq!(columns, 3, "row sheared by an unescaped pipe: {line}");
        }
    }
}
