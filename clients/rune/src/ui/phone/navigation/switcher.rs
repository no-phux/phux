//! A horizontal, read-only session preview, separate from the searchable list.
use rune::{El, col, el, row, text};

use super::{Desk, Look, Paint, Voice, icons};
use crate::ui::Cover;
use crate::ui::phone::{close_button, floating_round, glyph, hosted, sheet, touchable};

pub(super) fn view(desk: &Desk, top: f32, p: &Look) -> El {
    let s = &p.space;
    let r = &p.roles;
    // Where you are leads, as every sheet's title does; its close ends the
    // row, as every sheet's does, beside the way to the whole list.
    let all = floating_round(
        desk,
        "phone-switcher-list",
        "All sessions",
        &icons::SIDEBAR,
        "list.bullet",
        None,
        p,
    )
    .on_press(|d: &mut Desk| {
        d.close_phone_navigation();
        d.open_phone_sessions();
    });
    let close = close_button(desk, "phone-switcher-close", "Close switcher", p)
        .on_press(Desk::close_phone_navigation);
    let header = sheet::header(sheet::title("Switch session", p), [all, close], p);
    let body = match desk.switcher_selection() {
        Some((session, index, count)) => col()
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .gap(s.s3)
            .child(card(
                desk,
                p,
                desk.selected_session_name().as_deref() == Some(session.name.as_str()),
            ))
            .child(pager(desk, &session.name, index, count, p)),
        None => col().grow(1.0).center().child(
            text("Sessions changed. Open All sessions to browse again.")
                .font(p.ty.body)
                .wrap()
                .color(r.text.dim.paint()),
        ),
    };
    col()
        .key("phone-session-switcher")
        .cover()
        .fill(r.layer.ground.paint())
        .items_center()
        .pt(top)
        .pb(s.s3)
        .gap(s.s3)
        .label("Switch session")
        .modal(true, Desk::close_phone_navigation)
        .on_key(|d: &mut Desk, event, _| d.phone_navigation_key(event))
        .on_paste(|_: &mut Desk, _, _| {})
        .child(header.max_w(super::column_max()))
        .child(body.px(super::grid::margin()).max_w(super::column_max()))
}

fn card(desk: &Desk, p: &Look, current: bool) -> El {
    let page = desk
        .phone
        .as_ref()
        .and_then(|p| p.switcher.as_ref())
        .expect("switcher");
    let s = &p.space;
    let r = &p.roles;
    let (content, preview) = match page.preview.as_deref() {
        Some("") => ("Nothing on screen yet", false),
        Some(preview) => (preview, true),
        None if desk.switcher_loading() => ("Loading terminal preview…", false),
        None => ("No terminal preview", false),
    };
    let radius = p.radius.xl;
    let card = col()
        .key("phone-switcher-card")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(s.s4)
        .radius(radius)
        .clip_rounded(radius)
        .continuous_corners()
        .fill(r.layer.raised.paint())
        .border(
            s.hairline,
            if current { r.text.dim } else { r.border.base }.paint(),
        )
        // Follows a finger one to one, and settles on the page spring.
        .live_offset(page.slide.shift.smooth(p.motion.page.enter))
        .label("Open previewed session")
        .on_press(Desk::confirm_phone_switcher)
        .on_drag(|d: &mut Desk, drag, tx| d.switcher_drag(drag, tx));
    let ink = if preview { r.text.text } else { r.text.dim };
    col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .gap(s.s2)
        .child(
            touchable(card, p).child(
                text(content.to_owned())
                    .min_w(0.0)
                    .w_full()
                    .font(if preview { p.ty.mono } else { p.ty.body })
                    .color(ink.paint()),
            ),
        )
        .child(
            row()
                .min_h(s.s5)
                .px(s.s2)
                .gap(s.s2)
                .items_center()
                .child(glyph(
                    &icons::PROMPT,
                    p.icon.size.md,
                    p.roles.text.dim.paint(),
                ))
                .child(
                    text(page.caption.clone())
                        .font(p.ty.caption)
                        .truncate()
                        .color(p.roles.text.dim.paint())
                        .grow(1.0)
                        .min_w(0.0),
                ),
        )
}

fn pager(desk: &Desk, name: &str, index: usize, count: usize, p: &Look) -> El {
    let r = &p.roles;
    let s = &p.space;
    let dots = if count <= 7 {
        row().gap(s.s2).center().children((0..count).map(|at| {
            let fill = if at == index {
                r.text.text
            } else {
                r.border.base
            };
            el().size(s.s2, s.s2)
                .radius(p.radius.full)
                .fill(fill.paint())
        }))
    } else {
        text(format!("{} / {count}", index + 1))
            .font(p.ty.caption.figures())
            .color(r.text.dim.paint())
    };
    let label = format!("Open {name}, session {} of {count}", index + 1);
    let open = row()
        .key("phone-switcher-open")
        .grow(1.0)
        .min_w(0.0)
        .min_h(p.round())
        .center()
        .radius(p.radius.full)
        .px(s.s2)
        .label(&label)
        .on_press(Desk::confirm_phone_switcher)
        .child(
            text(name.to_owned())
                .font(p.ty.lead.semibold())
                .truncate()
                .min_w(0.0)
                .color(r.text.text.paint()),
        );
    let open = hosted(
        desk,
        touchable(open, p),
        crate::mobile::Control::painted("phone-switcher-open", &label, true),
    );
    let previous = step(
        desk,
        "phone-switcher-prev",
        "Previous session",
        &icons::CHEVRON_LEFT,
        -1,
        index > 0,
        p,
    );
    let next = step(
        desk,
        "phone-switcher-next",
        "Next session",
        &icons::CHEVRON_RIGHT,
        1,
        index + 1 < count,
        p,
    );
    col()
        .w_full()
        .gap(s.s2)
        .items_center()
        .child(super::super::bars::computer(desk, p))
        .child(
            row()
                .w_full()
                .gap(s.s2)
                .items_center()
                .child(previous)
                .child(open)
                .child(next),
        )
        .child(dots.a11y_hidden())
}

fn step(
    desk: &Desk,
    key: &'static str,
    label: &'static str,
    icon: &'static rune::Icon,
    direction: isize,
    enabled: bool,
    p: &Look,
) -> El {
    let ink = if enabled {
        p.roles.text.text
    } else {
        p.roles.text.faint
    };
    let button = super::super::round(key, label, glyph(icon, p.icon.size.lg, ink.paint()), p)
        .disabled(!enabled)
        .on_press(move |d: &mut Desk| d.step_phone_switcher(direction));
    hosted(
        desk,
        button,
        crate::mobile::Control::painted(key, label, enabled),
    )
}
