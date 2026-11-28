//! The Inbox's header: its title and count, and on a phone the round close
//! every sheet ends its header with and an answer's receipt.

use rune::{El, row, text};

use crate::desk::{Desk, NoticeKind};
use crate::ui::tokens::{Paint, Tokens};

/// The drawer's title and count. A phone has no Escape key, so it also
/// shows Close, a full touch target that dismisses as Escape does.
pub(super) fn title(desk: &Desk, count: usize, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    // A phone's title is every sheet's, in the phone's own type.
    let font = if desk.phone.is_some() {
        crate::ui::phone::sheet::title_type(t)
    } else {
        t.ty.title
    };
    let heading = crate::ui::questions::retirement_target(
        text("Inbox").font(font).color(r.text.text.paint()),
        crate::ui::questions::INBOX_FOCUS,
    );
    // A phone's empty drawer says so in its body; a zero here is noise.
    let count = (desk.phone.is_none() || count > 0).then(|| {
        text(count.to_string())
            .font(t.ty.readout)
            .color(r.text.dim.paint())
    });
    if desk.phone.is_none() {
        return row()
            .w_full()
            .h(s.s12)
            .px(s.s4)
            .items_center()
            .justify_between()
            .child(heading)
            .children(count);
    }
    let close = crate::ui::phone::close_button(desk, "inbox-close", "Close Inbox", t)
        .role("button")
        .on_press_tx(|d: &mut Desk, tx| {
            // The same dismissal as Escape, including clearing the draft.
            d.close_inbox();
            tx.blur();
        });
    // Confirmations stay on the title's own line, so they never grow the
    // header or displace the question above a keyboard; errors wrap in
    // the footer.
    let receipt = desk
        .notice
        .as_ref()
        .filter(|notice| notice.kind == NoticeKind::Info)
        .map(|notice| {
            text(notice.text.clone())
                .key("inbox-receipt-status")
                .role("status")
                .font(t.ty.caption)
                .color(r.text.text.paint())
                .grow(1.0)
                .min_w(0.0)
                .truncate()
        });
    let leading = row()
        .min_w(0.0)
        .gap(s.s2)
        .items_center()
        .child(heading)
        .children(count)
        .children(receipt);
    crate::ui::phone::sheet::header(leading, [close], t).key("inbox-header")
}
