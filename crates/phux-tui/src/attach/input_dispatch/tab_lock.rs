//! `lock-window` / `unlock-window`, and the refusal a locked tab's close
//! gets. The lock is server state (`ARRANGEMENT_LOCK`), written on the
//! dedicated control dial like session management; the arrangement change
//! it causes brings the flag back into the tab bar.

use phux_client::layout_ops::LayoutOps;

use crate::attach::connection::Connection;
use crate::render::overlay::ToastOverlay;

use super::ctx::DispatchCtx;
use super::effects::ActionEffects;
use super::run_action::ActionCall;

/// `lock-window`: lock the active tab against close.
pub(in crate::attach) fn lock_window(call: ActionCall<'_, '_>) {
    queue(call, true);
}

/// `unlock-window`: let the active tab close again.
pub(in crate::attach) fn unlock_window(call: ActionCall<'_, '_>) {
    queue(call, false);
}

fn queue(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>, locked: bool) {
    match ctx.workspace.groups.get(ctx.workspace.active) {
        Some(tab) => effects.tab_lock = Some((tab.id, locked)),
        None => effects.bell = true,
    }
}

/// Whether closing the active tab, or its only pane when `last_pane_only`,
/// is refused because the tab is locked; says so when it is.
pub(super) fn refuse_locked(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    last_pane_only: bool,
) -> bool {
    let Some(tab) = ctx.workspace.groups.get(ctx.workspace.active) else {
        return false;
    };
    let panes = tab
        .state
        .tree
        .as_ref()
        .map_or(0, |t| crate::layout::leaves(t).len());
    if !tab.locked || (last_pane_only && panes > 1) {
        return false;
    }
    effects.bell = true;
    ctx.overlays.push(Box::new(ToastOverlay::new(
        "Tab is locked",
        vec!["Unlock it first: Unlock in the tab's menu, or `unlock-window`.".to_owned()],
        ctx.theme,
    )));
    true
}

/// Send a queued lock or unlock, or say why it was not applied.
#[allow(clippy::future_not_send, reason = "TUI state is current-thread bound")]
pub(super) async fn apply(request: Option<([u8; 16], bool)>, ctx: &mut DispatchCtx<'_>) {
    let Some((group, locked)) = request else {
        return;
    };
    let result = async {
        let session = ctx.peers.focused_session.ok_or("No current session.")?;
        let dial = ctx
            .control_dial
            .ok_or("No dedicated session control dial; nothing sent.")?;
        let mut conn = Connection::connect_dial(dial)
            .await
            .map_err(|err| err.to_string())?;
        let read = LayoutOps::new(&mut conn, session, 0x5100)
            .read_arrangement()
            .await
            .map_err(|err| err.to_string())?;
        phux_client::org::lock_group(&mut conn, read.key, group, locked)
            .await
            .map(drop)
            .map_err(|err| err.to_string())
    }
    .await;
    if let Err(error) = result {
        ctx.overlays.push(Box::new(ToastOverlay::new(
            "Tab unchanged",
            vec![error],
            ctx.theme,
        )));
    }
}
