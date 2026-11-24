//! `lock-size` / `unlock-size`: claim or release this view's size lock
//! (`phux.view.size-lock/v1`), so other views stop resizing its panes.

use crate::attach::attach_role::{set_size_lock, size_lock_frame};
use crate::render::overlay::ToastOverlay;

use super::run_action::ActionCall;

/// `lock-size`: keep this view's pane sizes whoever else attaches.
pub(in crate::attach) fn lock_size(call: ActionCall<'_, '_>) {
    queue(call, true);
}

/// `unlock-size`: let the window-size policy size the panes again.
pub(in crate::attach) fn unlock_size(call: ActionCall<'_, '_>) {
    queue(call, false);
}

fn queue(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>, locked: bool) {
    set_size_lock(locked);
    effects
        .command_frames
        .push(size_lock_frame(ctx.take_request_id(), locked));
    let (title, body) = if locked {
        (
            "View size locked",
            "Other clients see these panes letterboxed or cropped.",
        )
    } else {
        (
            "View size unlocked",
            "defaults.window-size sizes the panes again.",
        )
    };
    ctx.overlays.push(Box::new(ToastOverlay::new(
        title,
        vec![body.to_owned()],
        ctx.theme,
    )));
}
