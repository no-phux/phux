//! Staged input, on its own surface below the last terminal row.
use rune::{El, col, el, row, text, text_input};

use super::{Look, glyph, icons, surface, touchable};
use crate::desk::Desk;
use crate::desk::phone::composer::{Draft, FIELD, Owner};
use crate::ui::tokens::{Paint, Voice};

pub(super) fn view(desk: &Desk, p: &Look) -> Option<El> {
    let owner = desk.active_composer()?.clone();
    let draft = desk.composer_draft(&owner)?;
    let target = super::bars::pane_title(desk);
    let hint = format!("Message {target}…");
    let card = editor(desk, owner, draft, hint, p);
    Some(el().w_full().px(p.space.s3).py(p.space.s2).child(card))
}

pub(super) fn toggle(desk: &Desk, active: bool, p: &Look) -> El {
    let r = &p.roles;
    let key = "phone-compose";
    let label = if active {
        "Hide composer"
    } else {
        "Show composer"
    };
    let button = el()
        .key(key)
        .size(p.round(), p.round())
        .center()
        .radius(p.radius.full)
        .label(label)
        .expanded(active)
        .preserve_focus_on_press(true)
        .fill(if active {
            r.wash.selected.paint()
        } else {
            rune::Color::TRANSPARENT
        })
        .on_press_tx(Desk::toggle_composer)
        .child(glyph(&icons::PENCIL, p.icon.size.md, r.text.text.paint()));
    super::hosted(
        desk,
        touchable(button, p),
        crate::mobile::Control::painted(key, label, true),
    )
}

fn editor(desk: &Desk, owner: Owner, draft: &Draft, hint: String, p: &Look) -> El {
    let r = &p.roles;
    let s = &p.space;
    let enter_owner = owner.clone();
    let field = text_input(&draft.edit, p.ty.body)
        .key(FIELD)
        .label("Terminal message")
        .w_full()
        .min_h(p.round())
        .color(r.text.text.paint())
        .placeholder(hint, r.text.dim.paint())
        .caret(r.accent.accent.paint(), r.wash.selected.paint())
        .wrap_anywhere()
        .on_edit(move |d: &mut Desk| {
            &mut d
                .phone
                .as_mut()
                .unwrap()
                .composer
                .draft_mut(&owner)
                .unwrap()
                .edit
        })
        .on_submit(move |d: &mut Desk, _| {
            if let Some(draft) = d
                .phone
                .as_mut()
                .and_then(|p| p.composer.draft_mut(&enter_owner))
            {
                draft.edit.insert("\n");
            }
        });
    let footer = row()
        .w_full()
        .items_center()
        .gap(s.s2)
        .child(
            text("Draft")
                .grow(1.0)
                .min_w(0.0)
                .pl(s.s2)
                .truncate()
                .font(p.ty.caption.medium())
                .color(r.text.dim.paint()),
        )
        .child(send(desk, draft, p));
    let card = col()
        .key("phone-composer")
        .w_full()
        .padding(s.s2)
        .gap(s.s1)
        .radius(p.radius.xl)
        .transition(rune::Transition::none())
        .preserve_focus_on_press(true)
        .child(el().w_full().px(s.s2).child(rune::auto_grow(field, 2, 3)))
        .child(footer);
    surface(card, p)
}

fn send(desk: &Desk, draft: &Draft, p: &Look) -> El {
    let busy = draft.sending.is_some();
    let enabled = draft.can_send();
    let r = &p.roles;
    let (fill, ink) = if enabled {
        (r.accent.accent, r.accent.on_accent)
    } else {
        (r.layer.sidebar, r.text.dim)
    };
    let key = "phone-compose-send";
    let label = if busy {
        "Sending draft"
    } else {
        "Send terminal message"
    };
    let button = touchable(
        el().key(key)
            .size(p.round(), p.round())
            .radius(p.radius.full)
            .center()
            .label(label)
            .disabled(!enabled)
            .on_press_tx(Desk::send_composer)
            .child(
                el().size(p.space.s8 + p.space.s1, p.space.s8 + p.space.s1)
                    .radius(p.radius.full)
                    .fill(fill.paint())
                    .center()
                    .child(glyph(&icons::ARROW_UP, p.icon.size.md, ink.paint())),
            ),
        p,
    );
    super::hosted(
        desk,
        button,
        crate::mobile::Control::painted(key, label, enabled),
    )
}
