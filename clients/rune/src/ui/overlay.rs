//! What floats over a pane: the find bar, a held-⌘ link's underline, and,
//! while history is scrolled back, a position rail and a way back to live.

use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::ResourceId;
use rune::{Color, El, Family, TextStyle, Transition, el, row, text, text_input};

use super::{CAPTION, Ink, PAD};
use crate::desk::Desk;
use crate::grid::Metrics;

const FIND_W: f32 = 340.0;
const PILL_W: f32 = 132.0;
const RAIL_W: f32 = 3.0;

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

fn icon_button(label: &'static str, key: &'static str, ink: &Ink, press: fn(&mut Desk)) -> El {
    el().key(key)
        .size(24.0, 24.0)
        .radius(5.0)
        .center()
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(press)
        .child(text(label).font(CAPTION).color(ink.dim))
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
    let case = el()
        .key("find-case")
        .size(26.0, 22.0)
        .radius(5.0)
        .center()
        .fill(if find.case_sensitive {
            ink.selected
        } else {
            Color::TRANSPARENT
        })
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(Desk::toggle_find_case)
        .child(text("Aa").font(CAPTION).color(if find.case_sensitive {
            ink.text
        } else {
            ink.dim
        }));
    let status = find.status(&desk.find_query.text);
    let miss = find.index.is_none() && !desk.find_query.text.is_empty();
    Some(
        row()
            .key("find-bar")
            .absolute(left, PAD + 4.0)
            .w(width)
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
                    Color::WHITE.alpha(0.08)
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
            .child(icon_button("↑", "find-older", ink, |d| d.step_find(-1)))
            .child(icon_button("↓", "find-newer", ink, |d| d.step_find(1)))
            .child(icon_button("✕", "find-close", ink, Desk::close_find)),
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
        .border(1.0, Color::WHITE.alpha(0.1))
        .shadow(6.0, 18.0, Color::BLACK.alpha(0.35))
        .cursor(rune::Cursor::Pointer)
        .transition(Transition::scale(0.9).combined(Transition::opacity()))
        .on_press(move |d: &mut Desk| d.follow_live(&target))
        .child(
            text(label)
                .font(CAPTION)
                .color(if unread > 0 { Color::WHITE } else { ink.text }),
        );
    vec![rail, pill]
}
