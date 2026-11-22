//! The chief of staff's composer: a scrim, a field the dictation app
//! pastes into, what the last send's receipt said, and the keys.

use rune::{El, col, el, row, text, text_input};

use super::overlay::{EXTENTS, Glass, glass, key_hint, pop};
use super::tokens::{Paint, Tier, Tokens};
use crate::desk::Desk;
use crate::ui::Cover;

/// `top` is the room above the panel, the palette's.
pub fn view(desk: &Desk, p: &Tokens, top: f32) -> El {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    let chief = &desk.chief;
    let name = desk
        .chief_session()
        .unwrap_or(crate::desk::chief::DEFAULT_SESSION);
    let held = chief.sending();
    let ink = if held { r.text.faint } else { r.text.text };
    let mut field = text_input(&chief.draft, t.ty.lead)
        .key("chief-draft")
        .label("Message for the chief of staff")
        .grow(1.0)
        .min_h(EXTENTS.chief_field_h)
        .color(ink.paint())
        .placeholder("Speak or type what needs doing…", r.text.faint.paint())
        .disabled(held);
    if !held {
        field = field
            .on_edit(|d: &mut Desk| &mut d.chief.draft)
            .on_submit(|d: &mut Desk, _| d.send_chief());
    }
    let header = row()
        .w_full()
        .px(s.s4)
        .pt(s.s3)
        .gap(s.s2)
        .items_center()
        .child(
            text("Chief of staff")
                .font(t.ty.label)
                .color(r.text.text.paint()),
        )
        .child(
            text(format!("session ‘{name}’ · typed ACP prompt"))
                .font(t.ty.caption)
                .color(r.text.dim.paint()),
        );
    let mut body = col()
        .w_full()
        .px(s.s4)
        .py(s.s3)
        .gap(s.s2)
        .child(super::inbox::field_shell(field, t, !held));
    if let Some(status) = &chief.status {
        body = body.child(
            text(status.clone())
                .key("chief-status")
                .font(t.ty.caption)
                .color(r.text.dim.paint())
                .wrap()
                .role("status"),
        );
    }
    if let Some(error) = &chief.error {
        // The invalid state's message: the failed tone, caption type.
        body = body.child(
            text(error.clone())
                .key("chief-error")
                .font(t.ty.caption)
                .color(r.status.failed.paint())
                .wrap()
                .role("alert"),
        );
    }
    let rule = rune::rule(s.hairline, r.border.base.paint())
        .w_full()
        .key("chief-footer-rule");
    let footer = row()
        .w_full()
        .px(s.s3)
        .py(s.s2)
        .gap(s.s3)
        .child(key_hint("↩", "send", t))
        .child(key_hint("⇧↩", "new line", t))
        .child(key_hint("esc", "close", t));
    let panel = col()
        .key("chief")
        .role("dialog")
        .label("Chief of staff")
        .w_full()
        .max_w(EXTENTS.palette_w)
        .shrink()
        .radius(t.radius.lg)
        .clip_rounded(t.radius.lg)
        .border(s.hairline, r.border.base.paint())
        .transition(pop(t))
        .child(header)
        .child(body)
        .child(rule)
        .child(footer);
    let panel = t.elevate(
        glass(panel, t, r.layer.overlay, Glass::Overlay, false),
        Tier::Overlay,
    );
    el().key("chief-scrim")
        .cover()
        .transition(rune::Transition::opacity())
        .modal(true, |d: &mut Desk| d.close_chief())
        .fill(r.layer.scrim.paint())
        .child(
            col()
                .w_full()
                .h_full()
                .pt(top)
                .px(s.s3)
                .pb(s.s3)
                .items_center()
                .child(panel),
        )
}

#[cfg(test)]
mod tests;
