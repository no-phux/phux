//! The action interpreter: one arm per canonical action name, plus the
//! action-finder overlay push.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::HashMap;

use phux_protocol::TerminalId;
use phux_protocol::wire::frame::{Command, FrameKind, InputMode};

use crate::attach::actions::{self, ActionError, PendingSplit, PendingWindow};
use crate::attach::pane_state::PaneSlot;
use crate::attach::plugin_panes::HostedPlacement;
use crate::layout::{SplitDir, Workspace};
use crate::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
use crate::render::overlay::{PromptOverlay, SelectItem, SelectList};

use super::args::{
    PaneMouseArg, amount_arg, direction_arg, focus_terminal, index_arg, mouse_arg, name_arg,
    ordered_workspace_panes, signal_arg, soft_kill_input_frames, split_dir_arg, str_arg, usize_arg,
};
use super::ctx::DispatchCtx;
use super::dispatch::{
    focused_pane_rect, open_context_menu, predicted_split_size, set_spawn_initial_size,
    spawn_initial_size,
};
use super::effects::{ActionEffects, ReattachTarget};
use super::pickers::{new_session_item, session_picker_items, switch_window, window_picker_items};

/// Open the single fuzzy discovery surface. `show-help` and
/// `command-palette` are entry aliases so users never have to choose between
/// a reference modal and an executable finder.
pub(super) fn push_action_finder(ctx: &mut DispatchCtx<'_>) {
    let items = crate::attach::action_registry::palette_items(
        ctx.keybindings,
        ctx.plugin_actions,
        ctx.plugin_panes,
    );
    ctx.overlays.push(Box::new(SelectList::new(
        "commands & help",
        items,
        ctx.theme,
    )));
}

/// Dispatch a resolved action against the driver's context.
///
/// Returns the [`ActionEffects`] the caller needs to apply. The function
/// is sync: it never touches the connection — frame I/O happens in the
/// caller (`dispatch_input_events`) so a hypothetical async wire-send
/// failure doesn't leave layout state half-mutated.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "per-action arms accrete one-by-one; splitting into per-action helpers would obscure the central dispatch table"
)]
pub(super) fn run_action(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&TerminalId>,
    // phux-foz.7: read-only view of the live pane slots. The `agent-fleet`
    // arm snapshots each pane's asked flag / OSC title / cwd from it;
    // every other arm ignores it. Threaded as a parameter (not a ctx
    // field) because the driver also passes `panes` mutably alongside the
    // ctx into `dispatch_input_events`.
    panes: &HashMap<TerminalId, PaneSlot>,
) -> ActionEffects {
    // One event per resolved action the user triggered. Info level: a
    // keybinding firing is a user-lifecycle event a trace reader wants under
    // the default filter, and it is human-paced (not per-frame), so it costs
    // nothing meaningful on the hot path. The action name is the key field;
    // any render-triggering effect is captured by the resulting repaint /
    // frame spans downstream.
    tracing::info!(action = %resolved.action, "input: running resolved action");
    let mut effects = ActionEffects::default();
    match resolved.action.as_str() {
        "split-pane" => {
            // phux-4li.12: SPAWN_TERMINAL → server allocates the new
            // Terminal under DEFAULT_GROUP_ID and replies with
            // TERMINAL_SPAWNED { request_id, result: Ok(new_id) }. The
            // layout mutation happens in the reply handler — see
            // `handle_server_frame`'s TerminalSpawned arm and
            // `apply_spawned_ok`. We park a `PendingSplit` keyed by
            // request id so the reply knows which leaf to split.
            let Some(dir) = split_dir_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "split-pane missing/bad `direction` arg (expected horizontal|vertical)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("split-pane: no focused pane to split against; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            let pending = PendingSplit {
                focused_at_request: focused_id,
                dir,
                zoom_on_spawn: false,
            };
            // CWD inheritance is phux-4li.1; until then we let the
            // server pick (typically $HOME). `command = None` invokes
            // the server's default shell; `env = None` inherits the
            // server's environment as-is.
            let frame = FrameKind::SpawnTerminal {
                request_id,
                group: DEFAULT_GROUP_ID,
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: predicted_split_size(ctx, &pending),
            };
            effects.spawn_terminal = Some((request_id, pending, frame));
        }
        "kill-pane" => {
            // phux-4li.12: soft-kill — write `exit\n` as a sequence of
            // INPUT_KEY events to the focused Terminal. When the shell
            // processes those keystrokes it exits, the PTY closes, and
            // the server broadcasts TERMINAL_CLOSED which we then fold
            // out of the layout in `handle_server_frame`.
            //
            // Caveat: this is softer than tmux's `kill-pane`, which
            // sends SIGKILL to the entire process group. If the
            // focused pane has an unresponsive foreground process
            // (e.g. a stuck `cat` blocked on a non-existent FIFO) the
            // keystrokes go nowhere. A future ticket may add an
            // explicit KILL_TERMINAL wire frame; for v0.1 this gets
            // the daily-drive flow working end-to-end.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("kill-pane: no focused pane to kill; dropping action");
                effects.bell = true;
                return effects;
            };
            effects.kill_frames = soft_kill_input_frames(&focused_id);
            // phux-i0e8.2.2: mark the close as ours so the resulting
            // TERMINAL_CLOSED does not raise a pane-exit notice.
            effects.expected_closes = vec![focused_id];
        }
        "take-input" => {
            // ADR-0033: seize the focused pane's input lease so only this
            // client's keystrokes reach the PTY. `Seize` preempts any holder;
            // the server broadcasts `TerminalControl` so the badge updates.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("take-input: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::AcquireInput {
                    terminal_id: focused_id,
                    mode: InputMode::Seize,
                    ttl_ms: 0,
                },
            });
        }
        "give-input" => {
            // ADR-0033: release the focused pane's input lease back to open
            // input. A no-op server-side if we do not hold it.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("give-input: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::ReleaseInput {
                    terminal_id: focused_id,
                },
            });
        }
        "signal-terminal" => {
            // ADR-0033: deliver a POSIX signal to the focused pane's process
            // group. `freeze`/`resume` is the reversible brake; distinct from
            // `kill-pane`, which removes the pane.
            let Some(signal) = signal_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "signal-terminal missing/bad `signal` arg (interrupt|freeze|resume|terminate|kill)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("signal-terminal: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::SignalTerminal {
                    terminal_id: focused_id,
                    signal,
                },
            });
        }
        "set-pane" => {
            // phux-npb3 (ADR-0048 decision 3 follow-up): flip the focused
            // pane's per-pane mouse opt-out. `mouse = "off"` opts the pane
            // out of client mouse handling (no synthesized INPUT_MOUSE; the
            // driver drops outer capture while the pane is focused, so the
            // host terminal's raw mouse handling returns for it alone);
            // `"on"` opts back in; `"toggle"` flips. Entirely client-local —
            // nothing crosses the wire.
            let Some(mode) = mouse_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "set-pane missing/bad `mouse` arg (expected on|off|toggle or a bool)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("set-pane: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let opt_out = match mode {
                PaneMouseArg::Off => true,
                PaneMouseArg::On => false,
                PaneMouseArg::Toggle => !ctx.mouse_optout.contains(&focused_id),
            };
            if opt_out {
                ctx.mouse_optout.insert(focused_id.clone());
            } else {
                ctx.mouse_optout.remove(&focused_id);
            }
            tracing::info!(
                terminal = ?focused_id,
                mouse = !opt_out,
                "set-pane: per-pane mouse opt-out updated"
            );
            // No repaint needed: the opt-out has no chrome today, and the
            // driver re-syncs the outer capture DECSET from this set at the
            // top of every loop iteration.
        }
        "new-window" => {
            // phux-4li.15: open a new window. Spawn a fresh Terminal
            // (same SPAWN as a split) and park a `PendingWindow`; the
            // reply (`handle_server_frame`'s TerminalSpawned arm) adds a
            // window seeded on the spawned pane and makes it active. The
            // new pane is a bare leaf — the server files it under the
            // default Group; the TUI groups it into a window itself
            // (windows are a client convention, ADR-0017).
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            let name = ctx.workspace.default_window_name();
            let frame = FrameKind::SpawnTerminal {
                request_id,
                group: DEFAULT_GROUP_ID,
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                // phux-a5xj: the new window holds one leaf, so the pane
                // fills the whole content rect. Predicting that here spares
                // the pane a bootstrap-then-reflow round trip.
                initial_size: spawn_initial_size(ctx, |content| Some((content.w, content.h))),
            };
            effects.spawn_window = Some((request_id, PendingWindow { name }, frame));
        }
        "kill-window" => {
            // phux-4li.15: soft-kill every pane in the active window, the
            // same `exit\n` mechanism as `kill-pane`. As each
            // TERMINAL_CLOSED lands, `handle_server_frame` folds the pane
            // out; when the window's tree empties it is pruned and the
            // new layout broadcast. No synchronous window removal here.
            let leaves = ctx
                .workspace
                .active_window()
                .and_then(|ls| ls.tree.as_ref().map(crate::layout::leaves))
                .unwrap_or_default();
            if leaves.is_empty() {
                tracing::warn!("kill-window: no active window to kill; dropping action");
                effects.bell = true;
                return effects;
            }
            effects.kill_frames = leaves.iter().flat_map(soft_kill_input_frames).collect();
            // phux-i0e8.2.2: every pane in the window dies at our request;
            // none of those closes is news.
            effects.expected_closes = leaves;
        }
        "next-window" => {
            switch_window(ctx, &mut effects, Workspace::next);
        }
        "previous-window" => {
            switch_window(ctx, &mut effects, Workspace::prev);
        }
        "select-window" => {
            let Some(index) = index_arg(resolved) else {
                tracing::warn!(args = ?resolved.args, "select-window missing/bad `index` arg");
                effects.bell = true;
                return effects;
            };
            switch_window(ctx, &mut effects, |w| {
                w.select(index);
            });
        }
        "rename-window" => {
            if ctx.workspace.active_window().is_none() {
                tracing::warn!("rename-window: no active window; dropping action");
                effects.bell = true;
                return effects;
            }
            if let Some(name) = name_arg(resolved) {
                // Explicit `name` renames immediately. A rename is shared
                // window state, so (unlike focus/switch) it broadcasts.
                ctx.workspace.rename_active(name);
                effects.layout_mutated = true;
                effects.set_metadata = true;
            } else {
                // No name ⇒ open the interactive prompt pre-filled with
                // the active window's current name. On commit it re-runs
                // `rename-window` with the typed name (phux-ahv.1).
                let current = ctx
                    .workspace
                    .windows
                    .get(ctx.workspace.active)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                ctx.overlays
                    .push(Box::new(PromptOverlay::rename_window(&current, ctx.theme)));
                effects.layout_mutated = true;
            }
        }
        "rename-session" => {
            // Rename the session this client is attached to. With an explicit
            // `name` it renames directly; with no name it opens a prompt
            // pre-filled with the current session name, which commits
            // `rename-session { name }` back through this same path (the
            // rename-window precedent). The actual `RENAME_SESSION` send +
            // optimistic local-name update happen in `apply_action_effects`
            // (the connection is async, run_action is sync — the `detach`
            // model).
            if let Some(name) = name_arg(resolved) {
                effects.rename_session = Some(name);
            } else {
                ctx.overlays.push(Box::new(PromptOverlay::rename_session(
                    ctx.session_name,
                    ctx.theme,
                )));
                effects.layout_mutated = true;
            }
        }
        "focus-direction" => {
            if let Some(dir) = direction_arg(resolved) {
                if let Some(ls) = ctx.workspace.active_window_mut()
                    && let Some(new_state) = actions::apply_focus(ls, dir)
                {
                    let new_focus = new_state.focus.clone();
                    *ls = new_state;
                    effects.layout_mutated = true;
                    effects.set_focus = new_focus;
                }
                // No-neighbour case: silently drop (tmux convention —
                // bumping into the layout edge isn't a bell).
            } else {
                tracing::warn!(args = ?resolved.args, "focus-direction missing/bad `direction` arg");
                effects.bell = true;
            }
        }
        "resize-pane" => {
            if let (Some(dir), Some(amount)) = (direction_arg(resolved), amount_arg(resolved)) {
                let Some(ls) = ctx.workspace.active_window_mut() else {
                    effects.bell = true;
                    return effects;
                };
                match actions::apply_resize(ls, dir, amount, ctx.viewport, ctx.sidebar) {
                    Ok(Some(new_state)) => {
                        *ls = new_state;
                        effects.layout_mutated = true;
                        effects.set_metadata = true;
                    }
                    Ok(None) | Err(ActionError::NoResizableBoundary) => {
                        // Underflow guard tripped or no matching axis —
                        // bell-no-op (ADR-0019 decision 5).
                        effects.bell = true;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "resize-pane failed");
                        effects.bell = true;
                    }
                }
            } else {
                tracing::warn!(args = ?resolved.args, "resize-pane missing args");
                effects.bell = true;
            }
        }
        "reload-config" => {
            // phux-foz.5: explicit live config reload. The actual re-read
            // + swap happens in the driver after this batch (see
            // `DispatchCtx::reload_request`): the resolver that just
            // resolved this chord, the theme, and the keybindings
            // snapshot are all borrowed by `ctx` right now — they are
            // exactly the state the reload replaces.
            effects.reload_config = true;
        }
        "show-help" | "command-palette" => push_action_finder(ctx),
        "getting-started" => {
            ctx.overlays
                .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
                    crate::attach::onboarding::ONBOARDING_TITLE,
                    crate::attach::onboarding::hint_lines(ctx.keybindings),
                    ctx.theme,
                )));
        }
        "copy-mode" => {
            // phux-wave-a-copy-mode: enter selection/copy mode. Arrow keys move
            // the cursor without extending the selection unless Shift is held;
            // mouse drag can select and copy in one gesture.
            let pane_rect = focused_pane_rect(ctx, focused);
            let overlay = Box::new(crate::render::overlay::CopyModeOverlay::new(
                0,
                0,
                pane_rect.w,
                pane_rect.h,
            ));
            ctx.overlays.push(overlay);
        }
        "context-menu" => {
            // phux-wrnm (ADR-0058): the keyboard route to the pane menu, and
            // the only route for a pane whose app owns the mouse. Anchored
            // just inside the focused pane's top-left corner so it opens over
            // the pane it acts on, wherever that pane sits in the layout.
            let rect = focused_pane_rect(ctx, focused);
            let anchor = (rect.x.saturating_add(2), rect.y.saturating_add(1));
            let zoomed = ctx.zoomed.is_some();
            let spec = crate::attach::context_menu::pane_menu(ctx.keybindings, zoomed);
            open_context_menu(ctx, spec, anchor);
        }
        "window-picker" => {
            // phux-4li.19 / nav: push the `<leader> w` grouped window
            // picker. Sessions are section headers; under the current
            // session each window (`index:name`, pane count) commits
            // `select-window { index }` (the same per-client switch the
            // numeric prefix bindings use). Other sessions list their own
            // windows as one-step `switch-session { name, window }` rows
            // when their persisted layout is cached (phux-foz.8), falling
            // back to a single "switch to session" row otherwise. With no
            // rows at all it bells.
            let items = window_picker_items(
                ctx.workspace,
                ctx.sessions,
                ctx.foreign_layouts,
                ctx.focused_session,
            );
            if items.iter().all(SelectItem::is_header) {
                effects.bell = true;
                return effects;
            }
            ctx.overlays
                .push(Box::new(SelectList::new("windows", items, ctx.theme)));
        }
        "session-picker" => {
            // phux-4li.20: push the session picker. The current session is
            // first and marked in its secondary text so the list is a full
            // inventory and opens with useful orientation. Committing that
            // row dismisses the picker as a silent no-op; peer rows commit
            // `switch-session { name }`. A trailing "+ New session" row
            // keeps creation reachable even when no sessions are cached.
            let mut items = session_picker_items(ctx.sessions, ctx.focused_session);
            items.push(new_session_item());
            ctx.overlays
                .push(Box::new(SelectList::new("sessions", items, ctx.theme)));
        }
        "agent-fleet" => {
            // phux-foz.7: push the agent-fleet dashboard — every pane of
            // the attached session grouped under session headers, with its
            // ADR-0040 agent record (name/kind + state glyph), ADR-0035
            // asked/attention highlight, and branch/cwd. Current-session
            // rows commit `focus-pane { window, pane }` through the single
            // dispatch path.
            //
            // phux-jpqd: a FOREIGN session with a cached persisted layout
            // (`foreign_layouts`) lists one row per pane committing a
            // one-step `switch-session { name, window, pane }`, its agent
            // glyph/state drawn from `foreign_agents` — no attach hop to see
            // a peer's panes. A foreign session with no cached layout still
            // falls back to a single `switch-session { name }` row.
            // Constructed with the fleet live key so the driver refreshes
            // the rows in place as agent events land while it is open. With
            // nothing to list it bells.
            let meta = crate::attach::fleet::collect_pane_meta(panes, ctx.vcs);
            let items = crate::attach::fleet::fleet_items(
                ctx.workspace,
                ctx.sessions,
                ctx.focused_session,
                ctx.agent_meta,
                &meta,
                ctx.foreign_layouts,
                ctx.foreign_agents,
            );
            if items.iter().all(SelectItem::is_header) {
                effects.bell = true;
                return effects;
            }
            ctx.overlays.push(Box::new(
                SelectList::new("agent fleet", items, ctx.theme)
                    .with_live_key(crate::attach::fleet::FLEET_LIVE_KEY),
            ));
        }
        "next-attention" => {
            // phux-oih5.16 / ADR-0049: advisory, client-local navigation over
            // asking panes. Flatten windows in display order and each tree in
            // DFS leaf order; choose the first asking pane strictly after the
            // current pane, wrapping once. No attention means a bell-no-op and
            // does not arm a return origin.
            let ordered = ordered_workspace_panes(ctx.workspace);
            let current = focused.and_then(|id| ordered.iter().position(|(_, pane)| pane == id));
            let target = ordered
                .iter()
                .enumerate()
                .filter(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
                .find(|(index, _)| current.is_none_or(|current| *index > current))
                .or_else(|| {
                    ordered
                        .iter()
                        .enumerate()
                        .find(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
                })
                .map(|(_, (window, id))| (*window, id.clone()));
            let Some((window, target)) = target else {
                effects.bell = true;
                return effects;
            };

            ctx.attention_navigation.save_origin_once(focused);
            focus_terminal(ctx.workspace, window, target.clone());
            effects.layout_mutated = true;
            effects.set_focus = Some(target);
        }
        "return-from-attention" => {
            // Consume first: a pane that disappeared while we were cycling is
            // a safe bell-no-op, not a sticky origin that can later resolve to
            // a different pane. TerminalId is stable across window reordering,
            // so a surviving origin is found in its current window/DFS slot.
            let Some(origin) = ctx.attention_navigation.take_origin() else {
                effects.bell = true;
                return effects;
            };
            let Some((window, _)) = ordered_workspace_panes(ctx.workspace)
                .into_iter()
                .find(|(_, id)| id == &origin)
            else {
                effects.bell = true;
                return effects;
            };
            focus_terminal(ctx.workspace, window, origin.clone());
            effects.layout_mutated = true;
            effects.set_focus = Some(origin);
        }
        "focus-pane" => {
            // phux-foz.7: focus a specific pane addressed as
            // (window index, DFS leaf ordinal) — the commit the fleet
            // dashboard's current-session rows carry. Per-client, like
            // `select-window` (no broadcast): switch to the window, then
            // move its client-local focus onto the target leaf. Stale
            // coordinates (the layout changed since the rows were built)
            // bell rather than focusing the wrong pane.
            let (Some(win), Some(ord)) =
                (usize_arg(resolved, "window"), usize_arg(resolved, "pane"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "focus-pane missing/bad `window`/`pane` args",
                );
                effects.bell = true;
                return effects;
            };
            let target = ctx
                .workspace
                .windows
                .get(win)
                .and_then(|w| w.state.tree.as_ref())
                .map(crate::layout::leaves)
                .and_then(|leaves| leaves.get(ord).cloned());
            let Some(target) = target else {
                tracing::warn!(
                    window = win,
                    pane = ord,
                    "focus-pane: no such pane (layout changed?)",
                );
                effects.bell = true;
                return effects;
            };
            switch_window(ctx, &mut effects, |w| {
                w.select(win);
            });
            if let Some(ls) = ctx.workspace.active_window_mut() {
                ls.focus = Some(target.clone());
            }
            effects.layout_mutated = true;
            effects.set_focus = Some(target);
        }
        "switch-session" => {
            // phux-4li.20 / phux-eb0: re-target this client to another
            // session. The effect carries the target up to
            // `apply_action_effects`, which routes it to the driver's
            // outer re-attach loop (in-process re-attach on the same
            // connection). A bad/absent `name` arg bells.
            //
            // phux-foz.8: an optional `window = N` arg makes it the
            // one-step cross-session window pick — after the re-attach
            // loads the target's persisted layout, the driver selects
            // window `N`. The grouped window picker's foreign-session
            // rows commit this form.
            //
            // phux-jpqd: an additional optional `pane = P` arg extends it
            // to a one-step cross-session PANE pick — after selecting the
            // window, the driver focuses its DFS leaf ordinal `P`. The
            // agent-fleet dashboard's foreign pane rows commit this form.
            if let Some(name) = name_arg(resolved) {
                let window = usize_arg(resolved, "window");
                let pane = usize_arg(resolved, "pane");
                effects.reattach = Some(ReattachTarget::Existing { name, window, pane });
            } else {
                tracing::warn!(
                    args = ?resolved.args,
                    "switch-session missing/bad `name` arg",
                );
                effects.bell = true;
            }
        }
        "new-session" => {
            // Create a fresh session (or attach to one already named) and
            // switch this client to it in-process. An explicit `name`
            // creates it directly; with no name we open a prompt to type
            // one, which commits `new-session { name }` back through this
            // same path. Either way the re-attach uses CreateIfMissing.
            match name_arg(resolved) {
                Some(name) => effects.reattach = Some(ReattachTarget::Create(name)),
                None => ctx
                    .overlays
                    .push(Box::new(PromptOverlay::new_session(ctx.theme))),
            }
        }
        "detach" => {
            effects.detach = true;
        }
        "plugin-action" => {
            // phux-r82.5: run a plugin manifest action through the same
            // child-process runtime `phux config run PLUGIN ACTION` uses.
            // Sync dispatch only records the intent; the async caller
            // (`apply_action_effects`) spawns the run off the input loop so
            // a slow plugin never freezes the TUI. Completion arrives on
            // the driver's plugin-events channel; failures toast.
            let (Some(plugin), Some(action)) =
                (str_arg(resolved, "plugin"), str_arg(resolved, "action"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "plugin-action missing/bad `plugin`/`action` args",
                );
                effects.bell = true;
                return effects;
            };
            effects.run_plugin = Some((plugin, action));
        }
        "plugin-pane" => {
            // phux-r82.7: open a plugin manifest `[[panes]]` entry as a
            // real server-side Terminal running the pane's argv. Routes
            // through the SAME SPAWN_TERMINAL machinery `split-pane` /
            // `new-window` use (ADR-0017: no plugin-privileged wire
            // surface) — the manifest supplies the command, the plugin
            // root the cwd, and PHUX_PLUGIN_* the additive env. Placement
            // picks the parked intent: `split`/`zoomed` park a
            // PendingSplit (zoomed also zooms the new pane when the
            // reply lands), `tab` parks a PendingWindow named after the
            // pane title. `overlay` entries never reach the snapshot
            // (deferred), so an unknown (plugin, pane) pair here also
            // covers a disabled plugin or an overlay declaration bound
            // directly in user config.
            let (Some(plugin), Some(pane)) =
                (str_arg(resolved, "plugin"), str_arg(resolved, "pane"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "plugin-pane missing/bad `plugin`/`pane` args",
                );
                effects.bell = true;
                return effects;
            };
            let Some(entry) = ctx
                .plugin_panes
                .iter()
                .find(|e| e.plugin_id == plugin && e.pane_id == pane)
            else {
                tracing::warn!(
                    plugin = %plugin,
                    pane = %pane,
                    "plugin-pane names no hostable pane (unknown, disabled, or overlay-deferred); dropping",
                );
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
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
                        return effects;
                    };
                    let pending = PendingSplit {
                        focused_at_request: focused_id,
                        // Side-by-side, matching the palette's
                        // `split-pane` default (vertical divider).
                        dir: SplitDir::Horizontal,
                        zoom_on_spawn: entry.placement == HostedPlacement::Zoomed,
                    };
                    set_spawn_initial_size(&mut frame, predicted_split_size(ctx, &pending));
                    effects.spawn_terminal = Some((request_id, pending, frame));
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
                        },
                        frame,
                    ));
                }
            }
        }
        "next-pane" => {
            if let Some(ls) = ctx.workspace.active_window_mut()
                && let Some(new_state) = actions::apply_next_pane(ls)
            {
                let new_focus = new_state.focus.clone();
                *ls = new_state;
                effects.layout_mutated = true;
                effects.set_focus = new_focus;
            }
        }
        "previous-pane" => {
            if let Some(ls) = ctx.workspace.active_window_mut()
                && let Some(new_state) = actions::apply_previous_pane(ls)
            {
                let new_focus = new_state.focus.clone();
                *ls = new_state;
                effects.layout_mutated = true;
                effects.set_focus = new_focus;
            }
        }
        "last-pane" => {
            // One-entry MRU jump-back. The target may be in another window;
            // locate it by stable TerminalId, switch the client-local active
            // window, and restore that window's local focus. Applying the
            // resulting focus change records the pane we jumped from as the
            // next MRU, so repeated invocations toggle between two panes.
            let Some(target) = ctx.focus_history.target(focused, ctx.workspace) else {
                effects.bell = true;
                return effects;
            };
            let owner = ctx.workspace.windows.iter().position(|window| {
                window
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| crate::layout::leaves(tree).contains(&target))
            });
            let Some(window) = owner else {
                tracing::debug!(terminal = ?target, "last-pane MRU target is no longer live");
                effects.bell = true;
                return effects;
            };
            ctx.workspace.active = window;
            ctx.workspace.windows[window].state.focus = Some(target.clone());
            effects.layout_mutated = true;
            effects.clear_predict = true;
            effects.set_focus = Some(target);
        }
        "toggle-zoom" => {
            // phux-x2hm: zoom needs more than one pane (a single-pane window
            // bells, like tmux). When already zoomed the REAL tree still has
            // >1 leaf, so this same check permits un-zooming. The driver owns
            // the `zoomed` state; we just signal intent + request a repaint.
            let multi = ctx
                .workspace
                .active_window()
                .and_then(|ls| ls.tree.as_ref())
                .is_some_and(|t| crate::layout::leaves(t).len() > 1);
            if multi {
                effects.toggle_zoom = true;
                effects.layout_mutated = true;
            } else {
                effects.bell = true;
            }
        }
        "toggle-sidebar" => {
            // The strip costs its width off every pane. On a terminal too
            // narrow to afford it and still leave a usable pane area, the
            // driver's reservation folds to `None` — so turning it "on"
            // would change nothing on screen and the keypress would read
            // as broken. Refuse with the bell instead, the same way zoom
            // refuses on a single-pane window: a refusal you can hear
            // beats a toggle that silently does nothing.
            //
            // Turning it *off* is always allowed: that direction never
            // needs room, and a user shrinking their terminal must be
            // able to reclaim the columns.
            let width = ctx.sidebar_width;
            if !*ctx.sidebar_enabled
                && ctx.viewport.0 < width.saturating_add(ctx.chrome.min_pane_cols)
            {
                effects.bell = true;
                return effects;
            }
            // phux-4h5a: show/hide the window sidebar. The driver owns
            // `sidebar_enabled`; we signal intent + a repaint so the panes
            // reflow into/out of the reserved columns.
            // phux-4h5a P4 follow-up: a `focus-window`-by-index action (the
            // keyboard companion to clicking a strip row) is deferred; the
            // existing `select-window` jumps by tab position, but a strip-row
            // index action that pairs with mouse click-to-focus is not yet
            // wired.
            effects.toggle_sidebar = true;
            effects.layout_mutated = true;
        }
        other => {
            tracing::debug!(action = other, "unhandled resolved action");
        }
    }
    effects
}
