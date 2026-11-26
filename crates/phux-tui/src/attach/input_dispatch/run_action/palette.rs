//! The command palette: one place to run anything and go anywhere.
//!
//! An empty palette leads with what this user ran lately, then every command
//! under its category, then every session's tabs. Typing ranks all of it at
//! once with the shared matcher, keeping each band under its header in the
//! order of its best match, so "api" lands on the session and "split" on the
//! command without choosing a mode first.

use crate::attach::input_dispatch::pickers::{WindowNames, window_picker_items};
use crate::attach::palette_recent;
use crate::render::overlay::{SelectItem, SelectList};

use super::ActionCall;

/// What the empty field says the palette can find.
const PLACEHOLDER: &str = "Search commands, sessions and tabs";

/// The live key the palette refreshes under when a host inventory lands
/// (sessions created since attach join the open palette).
pub(in crate::attach) const PALETTE_LIVE_KEY: &str = "command-palette";

/// Everything the palette's rows come from: the dispatch context on open,
/// the session loop on a refresh.
pub(in crate::attach) struct PaletteSources<'a> {
    pub keybindings: &'a phux_config::KeybindingsCfg,
    pub plugin_actions: &'a [phux_config::plugin::actions::PluginActionEntry],
    pub plugin_panes: &'a [crate::attach::plugin_panes::PluginPaneEntry],
    pub workspace: &'a crate::layout::ArrangementDoc,
    pub peers: &'a crate::attach::sidebar_zones::PeerInputs<'a>,
    pub names: WindowNames<'a>,
}

/// Open the command palette (`command-palette` and `show-help`) and ask
/// for a fresh host inventory, so sessions made since attach appear.
pub(in crate::attach) fn push_action_finder(ActionCall { ctx, panes, .. }: ActionCall<'_, '_>) {
    let items = palette_rows(&PaletteSources {
        keybindings: ctx.keybindings,
        plugin_actions: ctx.plugin_actions,
        plugin_panes: ctx.plugin_panes,
        workspace: ctx.workspace,
        peers: &ctx.peers,
        names: WindowNames {
            panes,
            agent_meta: ctx.agent_meta,
        },
    });
    *ctx.host_refresh_request = true;
    ctx.overlays.push(Box::new(
        SelectList::new("Commands", items, ctx.theme)
            .with_placeholder(PLACEHOLDER)
            .with_recents(palette_recent::default_path())
            .with_live_key(PALETTE_LIVE_KEY)
            .without_vi_navigation(),
    ));
}

/// The palette's rows: recent commands (empty query only), every command
/// under its category with a recency boost, then every session's tabs. The
/// recording marker shows only while a recording is live.
pub(in crate::attach) fn palette_rows(src: &PaletteSources<'_>) -> Vec<SelectItem> {
    let recent = palette_recent::load(&palette_recent::default_path());
    let mut commands = crate::attach::action_registry::palette_items(
        Some(src.keybindings),
        src.plugin_actions,
        src.plugin_panes,
    );
    if !phux_record::recording_live() {
        commands.retain(|item| item.action.action != "rec-mark");
    }
    for item in commands.iter_mut().filter(|item| !item.is_header()) {
        item.boost = palette_recent::boost(&recent, &item.action);
    }
    let places = window_picker_items(
        src.workspace,
        &src.names,
        src.peers.sessions,
        src.peers.foreign_layouts,
        src.peers
            .reports_sessions()
            .then(|| src.peers.terminal_counts())
            .as_ref(),
        src.peers.focused_session,
        src.peers.session_order,
    );
    let mut items = recent_rows(&recent, &commands);
    items.extend(commands);
    items.extend(named_switches(places));
    items
}

/// The tab picker's "switch to this session" row (a session whose tabs this
/// client has not loaded) says the session's name instead: in a mixed
/// palette the row has to stand on its own.
fn named_switches(places: Vec<SelectItem>) -> Vec<SelectItem> {
    let mut session = String::new();
    places
        .into_iter()
        .map(|mut item| {
            if item.is_header() {
                session.clone_from(&item.label);
            } else if item.action.action == "switch-session" && !session.is_empty() {
                item.label.clone_from(&session);
            }
            item
        })
        .collect()
}

/// A "Recent" band of the commands run lately (newest first), shown only
/// on an empty palette: a search finds each in its own section instead.
fn recent_rows(recent: &[String], commands: &[SelectItem]) -> Vec<SelectItem> {
    let rows: Vec<SelectItem> = recent
        .iter()
        .filter_map(|key| {
            commands
                .iter()
                .find(|item| !item.is_header() && palette_recent::key(&item.action) == *key)
        })
        .take(palette_recent::SHOWN)
        .map(|item| item.clone().browse_only())
        .collect();
    if rows.is_empty() {
        return rows;
    }
    std::iter::once(SelectItem::header("Recent"))
        .chain(rows)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> SelectItem {
        SelectItem::new(
            name,
            crate::attach::resolved(name, std::collections::BTreeMap::default()),
        )
        .indented()
    }

    #[test]
    fn recent_rows_lead_with_the_newest_known_commands_only() {
        let commands = vec![
            SelectItem::header("Pane"),
            row("split-pane"),
            row("new-window"),
        ];
        let recent = [
            "new-window".to_owned(),
            "gone".to_owned(),
            "split-pane".to_owned(),
        ];
        let rows = recent_rows(&recent, &commands);
        let labels: Vec<&str> = rows.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["Recent", "new-window", "split-pane"]);
        assert!(rows[1..].iter().all(|item| item.browse_only));
        assert!(
            recent_rows(&[], &commands).is_empty(),
            "no header without rows"
        );
    }
}
