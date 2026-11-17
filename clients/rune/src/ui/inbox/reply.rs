//! The picked question's reply: its field, and on a phone the named
//! Send beside it, pinned above the keyboard.

use rune::{El, col, text_input};
use rune_kit::Spinner;

use super::{field_shell, touch_input};
use crate::desk::Desk;
use crate::inbox::Item;
use crate::ui::icons;
use crate::ui::pressable;
use crate::ui::tokens::{Paint, Tokens};

/// The picked question's reply field.
pub(super) fn reply(desk: &Desk, t: &Tokens) -> El {
    let r = &t.roles;
    let phone = desk.phone.is_some();
    let font = t.ty.body;
    let input = text_input(&desk.inbox.reply, font)
        .key("inbox-reply")
        .label("Reply to question")
        .single_line()
        .grow(1.0)
        .color(r.text.text.paint())
        .placeholder("Reply…", r.text.faint.paint())
        .on_edit(|d: &mut Desk| &mut d.inbox.reply)
        .on_input_key(|d: &mut Desk, event, tx| d.inbox_key(event, tx))
        .on_submit(|d: &mut Desk, tx| d.submit_picked(tx));
    let field = field_shell(touch_input(input, font, phone, t), t, true);
    if !phone {
        return field;
    }
    field.gap(t.space.s2).pr(0.0).child(send_reply(desk, t))
}

/// A named Send beside the phone's editor, so Return is not its only
/// hint; it never takes Return's empty-draft "open terminal" fallback.
fn send_reply(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let r = &t.roles;
    let items = desk.inbox_items();
    let busy = (desk.picked_index(&items))
        .and_then(|i| items.get(i))
        .is_some_and(|item| matches!(item, Item::Terminal(w) if desk.answering(&w.terminal)));
    // Off, it takes the kit's disabled look (§1.10), never an opacity.
    let off = busy || desk.inbox.reply.text.trim().is_empty();
    let (fill, ink) = if off {
        (r.layer.sidebar.paint(), r.text.faint.paint())
    } else {
        (r.accent.accent.paint(), r.accent.on_accent.paint())
    };
    let mut send = pressable("inbox-send", t)
        .role("button")
        .label(if busy { "Sending answer" } else { "Send reply" })
        .size(s.touch_min, s.touch_min)
        .radius(s.touch_min / 2.0)
        .center()
        .fill(fill)
        .preserve_focus_on_press(true)
        .on_press(|d: &mut Desk| d.send_picked_reply());
    if off {
        send = send
            .disabled(true)
            .border(s.hairline, r.border.muted.paint());
    }
    let glyph = if busy {
        Spinner::new().size(t.icon.size.md).color(ink).build(&t.kit)
    } else {
        rune::icon(&icons::ARROW_UP, t.icon.size.md).color(ink)
    };
    send.child(glyph.a11y_hidden())
}

/// On a phone the picked question's reply stays pinned above the keyboard
/// while the questions scroll.
pub(super) fn phone_reply(
    desk: &Desk,
    items: &[Item],
    picked: Option<usize>,
    t: &Tokens,
) -> Option<El> {
    desk.phone.as_ref()?;
    let Item::Terminal(waiting) = items.get(picked?)? else {
        return None;
    };
    if !waiting.ask.as_ref()?.typed() {
        return None;
    }
    let s = &t.space;
    Some(col().w_full().px(s.s3).py(s.s2).child(reply(desk, t)))
}
