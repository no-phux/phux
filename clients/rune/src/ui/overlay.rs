//! What floats over the terminals: the find bar, a held-⌘ link's underline,
//! a toast, and, while history is scrolled back, a position rail and a way
//! back to live. Also the overlays' shared pieces: their extents, glass,
//! icon buttons and key hints.

use phux_client_runtime::publication::GridFrame;
use phux_design::{Rgba8, State};
use phux_protocol::ids::ResourceId;
use rune::{Edge, El, TextStyle, Transition, col, el, row, text, text_input};
use rune_kit::toast_stack::{ToastEntry, VISIBLE};
use rune_kit::{Button, ButtonVariant, ControlSize, SegmentedControl, ToastRegion};

use super::tokens::{MONO, Paint, Tier, Tokens};
use super::{PAD, badge, icon_press, icons, keep_from_pane};
use crate::desk::notice::ToastInput;
use crate::desk::{Desk, Notice, NoticeKind};
use crate::grid::Metrics;
use crate::host_color::{dot, named};

/// How large the overlays are: layout geometry, which no token group
/// holds (§1.6 `layout` names only the phone column and the reader). One
/// table, so a size is decided in one place.
pub(super) struct Extents {
    /// The command palette; the phone column's measure.
    pub(super) palette_w: f32,
    /// The path picker, narrower, at the cursor.
    pub(super) paths_w: f32,
    pub(super) paths_h: f32,
    /// How tall the palette's rows may grow before they scroll.
    pub(super) palette_rows_h: f32,
    pub(super) paths_rows_h: f32,
    /// The inbox drawer.
    pub(super) drawer_w: f32,
    /// A focused pane's ask card.
    pub(super) ask_w: f32,
    /// The narrowest pane an ask card still fits.
    pub(super) ask_min_w: f32,
    pub(super) find_w: f32,
    pub(super) find_min_w: f32,
    pub(super) toast_w: f32,
    /// The share of the window the popup takes, each way.
    pub(super) popup_share: f32,
    pub(super) news_w: f32,
}

pub(super) const EXTENTS: Extents = Extents {
    palette_w: phux_design::tokens::LAYOUT.content_max.phone_column,
    paths_w: 460.0,
    paths_h: 340.0,
    palette_rows_h: 380.0,
    paths_rows_h: 240.0,
    drawer_w: 400.0,
    ask_w: 340.0,
    ask_min_w: 160.0,
    find_w: 340.0,
    find_min_w: 160.0,
    toast_w: 420.0,
    popup_share: 0.8,
    news_w: 440.0,
};

/// Which glass surface a panel is (§1.9 `material.glass.where`).
#[derive(Clone, Copy)]
pub(super) enum Glass {
    /// The open modal surface: the palette, the inbox.
    Overlay,
    /// The find bar.
    Floating,
}

impl Glass {
    const fn key(self) -> &'static str {
        match self {
            Self::Overlay => "overlay",
            Self::Floating => "floating",
        }
    }

    const fn blur(self) -> f32 {
        let b = phux_design::tokens::MATERIAL.glass.blur;
        match self {
            Self::Overlay => b.overlay,
            Self::Floating => b.floating,
        }
    }
}

/// `panel` on `layer`, or glass over the terminal when the look puts glass
/// on this surface and the window lets it draw: the layer as a tint at the
/// `Settings.glass` alpha, never thinner than the verified floor (§1.9).
/// Opaque under increased contrast, reduce transparency and forced
/// colours, and on a phone, whose one blur is its dock.
pub(super) fn glass(panel: El, t: &Tokens, layer: Rgba8, surface: Glass, phone: bool) -> El {
    let opaque = layer.paint();
    let looked = phux_design::tokens::MATERIAL
        .glass
        .r#where
        .contains(&surface.key());
    let alpha = t.glass_tint_alpha(t.glass);
    if phone || !looked || alpha >= 1.0 {
        return panel.fill(opaque);
    }
    panel
        .backdrop_blur(surface.blur())
        .fill(t.alpha(opaque, alpha))
}

/// Exits fade; entries scale up from `enter_scale` on the pop spring (§1.8).
pub(super) fn pop(t: &Tokens) -> Transition {
    Transition::asymmetric(
        Transition::scale(t.motion.enter_scale).springs(t.motion.pop),
        Transition::opacity(),
    )
}

/// `style` one weight step heavier, within the type weights (§1.4).
pub(super) fn strong(style: TextStyle) -> TextStyle {
    let semibold = phux_design::tokens::TYPE.weights.semibold;
    TextStyle {
        weight: f32::from(u16::try_from(semibold).unwrap_or(u16::MAX)),
        ..style
    }
}

/// Numbers that change in place, in the chrome's mono face (§1.4 `tnum`).
pub(super) fn readout(t: &Tokens) -> TextStyle {
    TextStyle {
        family: MONO,
        ..t.ty.caption
    }
    .features("'tnum' 1")
}

/// The held-⌘ link's underline, in grid points: a hairline of the accent's
/// text ink.
pub fn link_underline(desk: &Desk, id: &ResourceId, m: &Metrics, p: &Tokens) -> Vec<El> {
    let t = p;
    let Some((_, link)) = desk.pointer.hover.as_ref().filter(|(t, _)| t == id) else {
        return Vec::new();
    };
    let thick = t.space.hairline;
    let ink = t.roles.accent.accent_text.paint();
    link.spans
        .iter()
        .map(|&(row, start, end)| {
            el().absolute(
                f32::from(start) * m.cell_w,
                f32::from(row + 1) * m.cell_h - thick,
            )
            .w(f32::from(end.saturating_sub(start)) * m.cell_w)
            .h(thick)
            .fill(ink)
            .inert()
        })
        .collect()
}

/// A key and what it does, as a sheet's footer lists them.
pub(crate) fn key_hint(key: &'static str, what: &'static str, p: &Tokens) -> El {
    let t = p;
    row()
        .gap(t.space.s1)
        .items_center()
        .child(super::keycap(key, p))
        .child(
            text(what)
                .font(t.ty.caption)
                .color(t.roles.text.dim.paint()),
        )
}

/// The machine a notice is about, when it names one.
fn named_machine<'a>(desk: &'a Desk, text: &str) -> Option<&'a str> {
    let hosts = desk.machines.hosts.iter().map(|host| host.name.as_str());
    named(text, std::iter::once(crate::machines::LOCAL).chain(hosts))
}

/// A notice's words, wrapping; read when it comes, an error at once. An
/// error carries the failed mark beside its words, never colour alone.
pub(super) fn notice_words(notice: &Notice, t: &Tokens) -> El {
    let (ink, role) = match notice.kind {
        NoticeKind::Info | NoticeKind::Tip => (t.roles.text.text, "status"),
        NoticeKind::Error => (t.roles.status.failed, "alert"),
    };
    let words = text(notice.text.clone())
        .role(role)
        .font(t.ty.caption)
        .color(ink.paint())
        .wrap()
        .grow(1.0)
        .min_w(0.0);
    match notice.kind {
        NoticeKind::Error => row()
            .gap(t.space.icon_gap)
            .items_center()
            .grow(1.0)
            .min_w(0.0)
            .child(badge::mark(t, State::Failed, t.icon.size.sm).a11y_hidden())
            .child(words),
        NoticeKind::Info | NoticeKind::Tip => words,
    }
}

/// The toasts, bottom left (top on a phone), clear of the live pill, in
/// the kit's notifications region: the front one alone while the stack is
/// collapsed, and up to [`VISIBLE`] of them, newest nearest the edge,
/// while the pointer holds it open (`Desk::toasts`). Timers pause while
/// it is open. The inbox's footer says the front one instead while the
/// drawer is open. Opaque: the house look keeps glass off toasts (§1.9
/// `where`).
pub fn toast(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    if desk.inbox.open {
        return None;
    }
    let toasts = desk.toasts();
    let front = toasts.front()?;
    let s = &t.space;
    let phone = desk.phone.is_some();
    let shown: Vec<_> = if toasts.expanded() {
        toasts.entries().iter().rev().take(VISIBLE).collect()
    } else {
        vec![front]
    };
    // Newest nearest the window's edge: the foot on a desktop, the top on
    // a phone.
    let lines = shown.into_iter().map(|entry| {
        let line = row()
            .w_full()
            .child(toast_card(desk, entry, entry.id == front.id, t));
        if phone { line.center() } else { line }
    });
    let lines: Vec<_> = if phone {
        lines.collect()
    } else {
        let mut v: Vec<_> = lines.collect();
        v.reverse();
        v
    };
    let stack = col()
        .key("toast-stack")
        .w_full()
        .gap(s.s2)
        .on_hover_change(|d: &mut Desk, inside: bool, tx: &mut rune::Tx| {
            let input = if inside {
                ToastInput::PointerOver
            } else {
                ToastInput::PointerLeft
            };
            d.toast_input(input, tx);
        })
        .children(lines);
    // A phone's prompt sits on the stage's last row, so a notice there goes
    // to the top instead, centred, where it is a passing word, not a place
    // to act.
    let (top, bottom) = if phone {
        (Some(s.s3), None)
    } else {
        (None, Some(s.s3))
    };
    let region =
        ToastRegion::new()
            .toast(stack)
            .build(&t.kit)
            .pin(Some(s.s3), top, Some(s.s3), bottom);
    Some(region)
}

/// One toast: its words (with the machine's dot when it names one), its
/// offer and its close. The front toast keeps the plain keys
/// (`toast`, `toast-offer`, `toast-close`); those behind it add their id.
fn toast_card(desk: &Desk, entry: &ToastEntry<Notice>, front: bool, t: &Tokens) -> El {
    let notice = &entry.options;
    let id = entry.id;
    let s = &t.space;
    let key = |base: &str| {
        if front {
            base.to_owned()
        } else {
            format!("{base}-{id}")
        }
    };
    let edge = match notice.kind {
        NoticeKind::Info | NoticeKind::Tip => t.roles.border.base,
        NoticeKind::Error => t.roles.status.failed_edge,
    };
    let phone = desk.phone.is_some();
    let ground = t.roles.layer.overlay.paint();
    let toast = row()
        .key(key("toast"))
        .on_drag(keep_from_pane)
        .shrink()
        .max_w(EXTENTS.toast_w)
        .min_h(s.row_h + s.s2)
        .pl(s.s3)
        .pr(s.s1)
        .py(s.s1)
        .gap(s.s2)
        .items_center()
        .radius(t.radius.md)
        .continuous_corners()
        .fill(ground)
        .border(s.hairline, edge.paint())
        .transition(Transition::asymmetric(
            Transition::move_edge(if phone { Edge::Top } else { Edge::Bottom })
                .springs(t.motion.pop),
            Transition::opacity(),
        ))
        .children(named_machine(desk, &notice.text).map(|host| dot(host, s.s2, ground)))
        .child(notice_words(notice, t));
    let toast = t.elevate(toast, Tier::Floating);
    let toast = match notice.action {
        Some((label, _)) => toast.child(
            Button::new(label)
                .variant(ButtonVariant::Neutral)
                .control_size(ControlSize::Small)
                .on_press(move |d: &mut Desk| d.take_offer_of(id))
                .build(&t.kit)
                .key(key("toast-offer")),
        ),
        None => toast,
    };
    toast.child(
        icon_press(
            &icons::CLOSE,
            "Dismiss",
            key("toast-close"),
            t,
            move |d: &mut Desk| d.dismiss_toast(id),
        )
        .help("Dismiss"),
    )
}

/// The find bar, top right of the pane find belongs to: a field, the
/// match count, case and the steps. A query that matches nothing is the
/// field's invalid state (§1.10).
pub fn find_bar(desk: &Desk, id: &ResourceId, area: [f32; 2], p: &Tokens) -> Option<El> {
    let t = p;
    let s = &t.space;
    let r = &t.roles;
    let find = desk
        .connection
        .find
        .as_ref()
        .filter(|f| &f.terminal == id)?;
    let width = EXTENTS.find_w.min(area[0] - s.s2).max(EXTENTS.find_min_w);
    let left = (PAD + area[0] - width - s.s1).max(0.0);
    let query = &desk.connection.find_query;
    let miss = find.index.is_none() && !query.text.is_empty();
    let field = text_input(query, t.ty.body)
        .key("find-query")
        .label("Find in terminal")
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .color(r.text.text.paint())
        .placeholder("Find", r.text.faint.paint())
        .invalid(miss)
        .on_edit(|d: &mut Desk| &mut d.connection.find_query)
        .on_input_key(|d: &mut Desk, event, tx| d.find_key(event, tx))
        .on_input_change(|d: &mut Desk, _| d.run_search());
    let case = SegmentedControl::new(&["Aa"], find.case_sensitive.then_some(0))
        .label("Match case")
        .fit()
        .on_pick(|d: &mut Desk, _, _| d.toggle_find_case())
        .build(&t.kit)
        .key("find-case");
    let status = find.status(&query.text);
    let (edge, count) = if miss {
        (r.status.failed, r.status.failed)
    } else {
        (r.border.base, r.text.dim)
    };
    let bar = row()
        .key("find-bar")
        .absolute(left, PAD + s.s1)
        .w(width)
        .on_drag(keep_from_pane)
        .min_h(s.row_h + s.s2)
        .pl(s.s3)
        .pr(s.s1)
        .gap(s.s1)
        .items_center()
        .radius(t.radius.md)
        .border(s.hairline, edge.paint())
        .transition(pop(t))
        .child(field)
        .child(text(status).font(readout(t)).color(count.paint()))
        .child(case)
        .child(
            icon_press(&icons::ARROW_UP, "Older match", "find-older", p, |d| {
                d.step_find(-1)
            })
            .help("Older match"),
        )
        .child(
            icon_press(&icons::ARROW_DOWN, "Newer match", "find-newer", p, |d| {
                d.step_find(1)
            })
            .help("Newer match"),
        )
        .child(
            icon_press(
                &icons::CLOSE,
                "Close find",
                "find-close",
                p,
                Desk::close_find,
            )
            .help("Close find"),
        );
    let bar = glass(
        bar,
        t,
        r.layer.overlay,
        Glass::Floating,
        desk.phone.is_some(),
    );
    Some(t.elevate(bar, Tier::Floating))
}

/// The pill's words: how much arrived while you were back in history.
fn live_words(unread: u64) -> String {
    match unread {
        0 => "Back to live".to_owned(),
        1 => "1 new line".to_owned(),
        n => format!("{n} new lines"),
    }
}

/// Where the viewport sits in history, and a pill back to the live tail,
/// only while scrolled back. The pill is a button: the primary fill while
/// lines wait, quiet otherwise.
pub fn history(
    desk: &Desk,
    id: &ResourceId,
    frame: &GridFrame,
    area: [f32; 2],
    p: &Tokens,
) -> Vec<El> {
    let t = p;
    let s = &t.space;
    let bar = frame.scrollbar;
    if bar.at_tail() || bar.total == 0 {
        return Vec::new();
    }
    let track = area[1];
    let thumb = (bar.len as f32 / bar.total as f32 * track).clamp(s.s6.min(track), track);
    // Not at the tail, so there is history above or below to travel.
    let travel = bar.total.saturating_sub(bar.len).max(1) as f32;
    let top = bar.offset as f32 / travel * (track - thumb);
    let rail_w = s.s1;
    let rail = el()
        .key(format!("rail-{id}"))
        .absolute(
            PAD + area[0] + (PAD - rail_w) / 2.0,
            PAD + top.clamp(0.0, track - thumb),
        )
        .w(rail_w)
        .h(thumb)
        .radius(t.radius.full)
        .fill(t.roles.border.strong.paint())
        .inert();
    let unread = desk.unread_rows(id);
    let target = id.clone();
    let variant = if unread > 0 {
        ButtonVariant::Submit
    } else {
        ButtonVariant::Neutral
    };
    let pill = Button::new(live_words(unread))
        .variant(variant)
        .icon(&icons::ARROW_DOWN)
        .on_press(move |d: &mut Desk| d.follow_live(&target))
        .build(&t.kit)
        .key(format!("live-{id}"))
        .pin(None, None, Some(PAD + s.s2), Some(PAD + s.s2))
        .transition(pop(t));
    vec![rail, t.elevate(pill, Tier::Floating)]
}

/// A column of words a surface shows in place of content.
pub(super) fn empty_note(words: impl Into<String>, t: &Tokens) -> El {
    col().w_full().py(t.space.s4).items_center().child(
        text(words.into())
            .font(t.ty.caption)
            .color(t.roles.text.dim.paint()),
    )
}

#[cfg(test)]
mod tests;
