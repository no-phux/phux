//! `sync-panes`: type into every pane of the tab at once.

use super::ActionCall;

/// Turn synchronized input on for the tab in view, or off. Said out loud
/// each time, because typing into panes you are not looking at is the kind
/// of thing to know about.
pub(in crate::attach) fn sync_panes(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    let on = crate::attach::sync_input::toggle(ctx.workspace);
    *ctx.rename_notice = Some(
        if on {
            "typing now goes to every pane in this tab; run sync-panes again to stop"
        } else {
            "typing goes to the focused pane only"
        }
        .to_owned(),
    );
}
