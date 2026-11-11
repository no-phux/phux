//! What floats over the terminals: the find bar, a held-⌘ link's underline,
//! a toast, and, while history is scrolled back, a position rail and a way
//! back to live.

use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::ResourceId;
use rune::{Color, El, Family, TextStyle, Transition, el, row, text, text_input};

use super::status::notice_line;
use super::{CAPTION, Ink, PAD, icons, keep_from_pane, pressable};
use crate::desk::{Desk, NoticeKind};
use crate::grid::Metrics;
use crate::host_color::{dot, named};

const FIND_W: f32 = 340.0;
const PILL_W: f32 = 132.0;
const RAIL_W: f32 = 3.0;
const TOAST_W: f32 = 420.0;

/// The held-⌘ link's underline, in grid points.
pub fn link_underline(desk: &Desk, id: &ResourceId, m: &Metrics, ink: &Ink) -> Vec<El> {
    let Some((_, link)) = desk.pointer.hover.as_ref().filter(|(t, _)| t == id) else {
        return Vec::new();
    };
    link.spans
        .iter()
        .map(|&(row, start, end)| {
            el().absolute(
                f32::from(start) * m.cell_w,
                f32::from(row + 1) * m.cell_h - 1.5,
            )
            .size(f32::from(end.saturating_sub(start)) * m.cell_w, 1.5)
            .fill(ink.accent)
            .inert()
        })
        .collect()
}

/// An icon to press; `name` is what a screen reader says for it.
pub(super) fn icon_button(
    glyph: &'static rune::Icon,
    name: &'static str,
    key: impl Into<String>,
    ink: &Ink,
    press: fn(&mut Desk),
) -> El {
    pressable(key, ink)
        .size(24.0, 24.0)
        .radius(6.0)
        .center()
        .label(name)
        .help(name)
        .on_press(press)
        .hover_group()
        .child(
            rune::icon(glyph, 14.0)
                .color(ink.dim)
                .group_color(ink.text)
                .a11y_hidden(),
        )
}

/// A key and what it does, as a sheet's footer lists them.
pub(super) fn key_hint(key: &'static str, what: &'static str, ink: &Ink) -> El {
    row()
        .gap(5.0)
        .items_center()
        .child(super::keycap(key, ink))
        .child(text(what).font(CAPTION).color(ink.dim))
}

/// The current notice, bottom left (top on a phone), clear of the live pill. The inbox's
/// footer says it instead while the drawer is open.
/// The machine a notice is about, when it names one.
fn named_machine<'a>(desk: &'a Desk, text: &str) -> Option<&'a str> {
    let hosts = desk.machines.hosts.iter().map(|host| host.name.as_str());
    named(text, std::iter::once(crate::machines::LOCAL).chain(hosts))
}

pub fn toast(desk: &Desk, ink: &Ink) -> Option<El> {
    let notice = desk.notice.as_ref().filter(|_| !desk.inbox.open)?;
    let edge = match notice.kind {
        NoticeKind::Info => ink.separator,
        NoticeKind::Tip => ink.accent.alpha(0.6),
        NoticeKind::Error => ink.danger.alpha(0.6),
    };
    let toast = row()
        .key("toast")
        .on_drag(keep_from_pane)
        .shrink()
        .max_w(TOAST_W)
        .pl(12.0)
        .pr(4.0)
        .py(4.0)
        .gap(8.0)
        .items_center()
        .radius(9.0)
        .continuous_corners()
        // Frosted: the terminal under it blurs through a tint of the chrome.
        .backdrop_blur(super::FROST)
        .fill(ink.raised.alpha(0.82))
        .border(1.0, edge)
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
        .transition(Transition::opacity())
        .children(named_machine(desk, &notice.text).map(|host| dot(host, 8.0, ink.raised)))
        .child(notice_line(Some(notice), ink));
    let toast = match notice.action {
        Some((label, _)) => toast.child(
            pressable("toast-offer", ink)
                .label(label)
                .px(8.0)
                .py(3.0)
                .radius(6.0)
                .border(1.0, ink.separator)
                .on_press(Desk::take_offer)
                .child(text(label).font(CAPTION).color(ink.text)),
        ),
        None => toast,
    };
    let toast = toast.child(icon_button(
        &icons::CLOSE,
        "Dismiss",
        "toast-close",
        ink,
        Desk::dismiss_notice,
    ));
    // The host gives the words a width to wrap at. A phone's prompt sits on
    // the stage's last row, so a notice there goes to the top instead.
    let (top, bottom) = if desk.phone.is_some() {
        (Some(12.0), None)
    } else {
        (None, Some(12.0))
    };
    let line = row().pin(Some(12.0), top, Some(12.0), bottom);
    // Centred on a phone, where it is a passing word, not a place to act.
    Some(if desk.phone.is_some() {
        line.center().child(toast)
    } else {
        line.child(toast)
    })
}

/// The find bar, top right of the pane find belongs to.
pub fn find_bar(desk: &Desk, id: &ResourceId, area: [f32; 2], ink: &Ink) -> Option<El> {
    let find = desk.find.as_ref().filter(|f| &f.terminal == id)?;
    let width = FIND_W.min(area[0] - 8.0).max(160.0);
    let left = (PAD + area[0] - width - 6.0).max(0.0);
    let field = text_input(&desk.find_query, TextStyle::new(13.0, 400.0).lh(20.0))
        .key("find-query")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(ink.text)
        .placeholder("Find", ink.dim)
        .on_edit(|d: &mut Desk| &mut d.find_query)
        .on_input_key(|d: &mut Desk, event, tx| d.find_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.run_search());
    let case = pressable("find-case", ink)
        .size(26.0, 22.0)
        .radius(5.0)
        .center()
        .fill(if find.case_sensitive {
            ink.selected
        } else {
            Color::TRANSPARENT
        })
        .role("switch")
        .label("Match case")
        .checked(find.case_sensitive)
        .on_press(Desk::toggle_find_case)
        .child(
            text("Aa")
                .font(CAPTION)
                .color(if find.case_sensitive {
                    ink.text
                } else {
                    ink.dim
                })
                .a11y_hidden(),
        );
    let status = find.status(&desk.find_query.text);
    let miss = find.index.is_none() && !desk.find_query.text.is_empty();
    Some(
        row()
            .key("find-bar")
            .absolute(left, PAD + 4.0)
            .w(width)
            .on_drag(keep_from_pane)
            .h(36.0)
            .pl(12.0)
            .pr(4.0)
            .gap(4.0)
            .items_center()
            .radius(9.0)
            .fill(ink.raised)
            .border(
                1.0,
                if miss {
                    ink.danger.alpha(0.6)
                } else {
                    ink.separator
                },
            )
            .shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
            .transition(Transition::scale(0.97).combined(Transition::opacity()))
            .child(field)
            .child(
                text(status)
                    .font(CAPTION.family(Family::SystemMono))
                    .color(if miss { ink.danger } else { ink.dim }),
            )
            .child(case)
            .child(icon_button(
                &icons::ARROW_UP,
                "Older match",
                "find-older",
                ink,
                |d| d.step_find(-1),
            ))
            .child(icon_button(
                &icons::ARROW_DOWN,
                "Newer match",
                "find-newer",
                ink,
                |d| d.step_find(1),
            ))
            .child(icon_button(
                &icons::CLOSE,
                "Close find",
                "find-close",
                ink,
                Desk::close_find,
            )),
    )
}

/// Where the viewport sits in history, and a pill back to the live tail,
/// only while scrolled back.
pub fn history(
    desk: &Desk,
    id: &ResourceId,
    frame: &GridFrame,
    area: [f32; 2],
    ink: &Ink,
) -> Vec<El> {
    let bar = frame.scrollbar;
    if bar.at_tail() || bar.total == 0 {
        return Vec::new();
    }
    let track = area[1];
    let thumb = (bar.len as f32 / bar.total as f32 * track).clamp(24.0_f32.min(track), track);
    // Not at the tail, so there is history above or below to travel.
    let travel = bar.total.saturating_sub(bar.len).max(1) as f32;
    let top = bar.offset as f32 / travel * (track - thumb);
    let rail = el()
        .key(format!("rail-{id}"))
        .absolute(
            PAD + area[0] + (PAD - RAIL_W) / 2.0,
            PAD + top.clamp(0.0, track - thumb),
        )
        .size(RAIL_W, thumb)
        .radius(RAIL_W / 2.0)
        .fill(ink.dim.alpha(0.55))
        .inert();
    let unread = desk.unread_rows(id);
    let label = match unread {
        0 => "↓  Back to live".to_owned(),
        1 => "↓  1 new line".to_owned(),
        n => format!("↓  {n} new lines"),
    };
    let target = id.clone();
    let pill = el()
        .key(format!("live-{id}"))
        .absolute(PAD + area[0] - PILL_W - 10.0, PAD + area[1] - 38.0)
        .size(PILL_W, 28.0)
        .radius(14.0)
        .center()
        .fill(if unread > 0 { ink.accent } else { ink.raised })
        .border(1.0, ink.separator)
        .shadow(6.0, 18.0, Color::BLACK.alpha(0.35))
        .cursor(rune::Cursor::Pointer)
        .transition(Transition::scale(0.9).combined(Transition::opacity()))
        .on_press(move |d: &mut Desk| d.follow_live(&target))
        .child(
            text(label)
                .font(CAPTION)
                .color(if unread > 0 { ink.on_accent } else { ink.text }),
        );
    vec![rail, pill]
}
