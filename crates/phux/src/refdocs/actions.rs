//! The generated actions reference: every dispatcher action with its
//! parameter surface, description, and palette placement.
//!
//! Three in-code inventories feed the page, and tests keep them a
//! partition of the real action set:
//!
//! - `phux_config::vocab::ACTION_NAMES` is the canonical name list (and
//!   this page's row order);
//! - `phux_client::attach::action_registry::REGISTRY` supplies the
//!   description, parameter surface, and palette category for every
//!   palette-offered action;
//! - `phux_client::attach::action_registry::NON_PALETTE_ACTIONS` supplies
//!   the same for the deliberate palette omissions, plus the reason each
//!   has no palette row.
//!
//! The `every_action_has_exactly_one_doc_home` test in phux-client pins
//! the union of the last two to the first, so a new `run_action` arm
//! cannot ship without a doc blurb — and the freshness test in
//! `super::tests` then forces this page's regeneration.

use phux_client::attach::action_registry::{NON_PALETTE_ACTIONS, REGISTRY};
use phux_config::vocab::ACTION_NAMES;

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
    for &name in ACTION_NAMES {
        if let Some(spec) = REGISTRY.iter().find(|spec| spec.name == name) {
            let _ = writeln!(
                body,
                "| `{name}` | {} | {} | {} |",
                spec.category.header(),
                cell(spec.params),
                cell(spec.description),
            );
        } else if let Some(spec) = NON_PALETTE_ACTIONS.iter().find(|spec| spec.name == name) {
            let _ = writeln!(
                body,
                "| `{name}` | — | {} | {} |",
                cell(spec.params),
                cell(spec.description),
            );
        }
        // A name in neither inventory is unreachable: the partition test
        // in phux-client fails first.
    }

    body.push_str("\nWhy the dash rows have no palette entry:\n\n");
    for spec in NON_PALETTE_ACTIONS {
        let _ = writeln!(body, "- `{}` — {}.", spec.name, spec.reason);
    }

    Page {
        file: "actions.md",
        title: "phux actions reference",
        summary: "Every dispatcher action with its parameters, description, \
                  and palette placement.",
        tldr: "Every action a keybinding, palette row, context menu, or \
               hook can dispatch, with its parameter surface and where the \
               command palette offers it. Rendered from the same in-code \
               inventories the dispatcher and palette are test-pinned to, \
               so an action is listed here exactly when the binary handles \
               it.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{ACTION_NAMES, page};

    /// The page carries one table row per canonical action name — the
    /// page-level face of the partition test in phux-client.
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
