//! Plugin handlers: manifest actions run as jobs, manifest panes opened as
//! terminals.

use crate::attach::actions::{PendingSplit, PendingWindow, SplitHost};
use crate::attach::input_dispatch::args::str_arg;
use crate::attach::input_dispatch::dispatch::{
    predicted_split_size, set_spawn_initial_size, spawn_initial_size,
};
use crate::attach::plugin_actions;
use crate::attach::plugin_panes::HostedPlacement;
use crate::layout::SplitDir;

use super::ActionCall;

/// Hand the effect pass a plugin action run as a background job (ADR-0172).
/// The action id is `id` (what a config binding can spell), or the older
/// `action` from callers that build the args directly.
pub(in crate::attach) fn plugin_action(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    let Some((plugin, action_id)) = plugin_actions::plugin_action_target(&resolved.args) else {
        tracing::warn!(
            args = ?resolved.args,
            "plugin-action missing/bad `plugin`/`id` args",
        );
        effects.bell = true;
        return;
    };
    effects.job = Some(plugin_actions::plugin_job(
        plugin.to_owned(),
        action_id.to_owned(),
    ));
}

/// Open a plugin `[[panes]]` entry as a Terminal via the ordinary spawn
/// (ADR-0017): `split`/`zoomed` park a split, `tab` a window, `overlay` a
/// floating box (ADR-0147). An unknown pair (disabled, typo) bells.
pub(in crate::attach) fn plugin_pane(
    ActionCall {
        resolved,
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let (Some(plugin), Some(pane)) = (str_arg(resolved, "plugin"), str_arg(resolved, "pane"))
    else {
        tracing::warn!(
            args = ?resolved.args,
            "plugin-pane missing/bad `plugin`/`pane` args",
        );
        effects.bell = true;
        return;
    };
    let Some(entry) = ctx
        .plugin_panes
        .iter()
        .find(|e| e.plugin_id == plugin && e.pane_id == pane)
    else {
        tracing::warn!(
            plugin = %plugin,
            pane = %pane,
            "plugin-pane names no hostable pane (unknown or disabled); dropping",
        );
        effects.bell = true;
        return;
    };
    let request_id = ctx.take_request_id();
    let mut frame = entry.spawn_frame(request_id);
    match entry.placement {
        HostedPlacement::Split | HostedPlacement::Zoomed => {
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!(
                    plugin = %plugin,
                    pane = %pane,
                    "plugin-pane split/zoomed placement needs a focused pane; dropping",
                );
                effects.bell = true;
                return;
            };
            let pending = PendingSplit {
                focused_at_request: focused_id,
                // Side-by-side, matching the palette's
                // `split-pane` default (vertical divider).
                dir: SplitDir::Horizontal,
                zoom_on_spawn: entry.placement == HostedPlacement::Zoomed,
                host: SplitHost::Attached,
                adopt: None,
                open_existing: None,
            };
            set_spawn_initial_size(&mut frame, predicted_split_size(ctx, &pending));
            effects.spawn_terminal = Some((request_id, pending, frame));
        }
        HostedPlacement::Overlay => {
            // ADR-0147: sized to the box interior; no window adopts it.
            set_spawn_initial_size(
                &mut frame,
                spawn_initial_size(ctx, |content| {
                    let inner = crate::attach::floating::floating_box(content).inner;
                    Some((inner.w, inner.h))
                }),
            );
            effects.spawn_floating = Some((request_id, entry.title.clone(), frame));
        }
        HostedPlacement::Tab => {
            set_spawn_initial_size(
                &mut frame,
                spawn_initial_size(ctx, |content| Some((content.w, content.h))),
            );
            effects.spawn_window = Some((
                request_id,
                PendingWindow {
                    name: entry.title.clone(),
                    adopt: None,
                },
                frame,
            ));
        }
    }
}
