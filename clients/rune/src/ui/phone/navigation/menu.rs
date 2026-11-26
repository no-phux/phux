//! The long-press menu: what was held, named at the top, then its verbs as
//! touch rows with a glyph at the trailing edge and the destructive one last
//! in the failure tone, floating beside the finger over a dimmed page.
use crate::ui::tokens::Voice;
use phux_design::State;
use rune::{El, col, el, text};

use super::{Desk, Look, card, control, nested, separator};
use crate::desk::phone::navigation::{Held, Verb};
use crate::ui::Cover;
use crate::ui::icons;
use crate::ui::tokens::{Paint, Tier};
use phux_protocol::ids::ResourceId;

pub(super) fn view(desk: &Desk, p: &Look) -> Option<El> {
    // The native host opens the system's own menu from the held row.
    if desk.native.is_some() {
        return None;
    }
    let phone = desk.phone.as_ref()?;
    let menu = phone.menu.as_ref()?;
    let (title, detail, verbs) = contents(desk, &menu.held)?;
    let s = &p.space;
    let margin = crate::ui::tokens::phone::page_margin();
    let [page_w, page_h] = phone.page_size;
    let width = crate::ui::tokens::layout::PHONE
        .menu_w
        .min(page_w - 2.0 * margin)
        .max(0.0);
    let [x, y] = menu.at;
    // Beside the held row, never over it or off the page: it hangs below a
    // row in the upper half and stands above one in the lower half, and
    // scrolls if large text makes it taller than that room. Before the row
    // has a rect to read, the finger stands in for it.
    let [_, row_y, _, row_h] = rune::memo::window_layout(&menu.row).unwrap_or([x, y, 0.0, 0.0]);
    let left = (x - width / 2.0).clamp(margin, (page_w - margin - width).max(margin));
    let (place, room) = if row_y + row_h / 2.0 < page_h / 2.0 {
        let top = row_y + row_h + s.s2;
        ((Some(top), None), page_h - margin - top)
    } else {
        let bottom = page_h - row_y + s.s2;
        ((None, Some(bottom)), page_h - margin - bottom)
    };
    let mut surface = card(p)
        .key("phone-menu")
        .w(width)
        .max_h(room.max(s.touch_min))
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .role("menu")
        .label(title.clone())
        .child(
            col()
                .w_full()
                .px(s.s3)
                .py(s.s2)
                .gap(s.s0_5)
                .child(
                    text(title)
                        .font(p.ty.label.semibold())
                        .color(p.roles.text.text.paint())
                        .truncate(),
                )
                .children((!detail.is_empty()).then(|| {
                    text(detail)
                        .font(p.ty.caption)
                        .color(p.roles.text.dim.paint())
                        .truncate()
                })),
        );
    for (verb, name, glyph, _) in verbs {
        surface = surface
            .child(separator(p))
            .child(verb_row(verb, name, glyph, p));
    }
    let surface = p
        .elevate(surface, Tier::Overlay)
        .pin(Some(left), place.0, None, place.1);
    Some(
        el().key("phone-menu-scrim")
            .cover()
            .fill(p.roles.layer.scrim.paint())
            .transition(rune::Transition::opacity())
            .modal(true, Desk::close_phone_menu)
            .on_press(Desk::close_phone_menu)
            .label("Menu")
            .child(surface),
    )
}

/// Each verb, its name, Rune's glyph for it and the SF Symbol the native
/// host's menu shows instead.
type Verbs = Vec<(Verb, String, &'static rune::Icon, &'static str)>;

/// Each verb of the native host's menu, by name and SF Symbol.
pub(crate) type NativeVerbs = Vec<(Verb, String, &'static str)>;

/// The menu the native host opens in place of Rune's: the held thing's
/// name and detail, then its verbs.
pub(crate) fn native(desk: &Desk) -> Option<(String, String, NativeVerbs)> {
    let menu = desk.phone.as_ref()?.menu.as_ref()?;
    let (title, detail, verbs) = contents(desk, &menu.held)?;
    let verbs = verbs
        .into_iter()
        .map(|(verb, name, _, symbol)| (verb, name, symbol));
    Some((title, detail, verbs.collect()))
}

/// The held thing's name, its one-line detail and what can be done to it.
fn contents(desk: &Desk, held: &Held) -> Option<(String, String, Verbs)> {
    match held {
        Held::Session(id) => session_menu(desk, *id),
        Held::Pane(id, session) => pane_menu(desk, id, session),
    }
}

fn session_menu(desk: &Desk, id: u32) -> Option<(String, String, Verbs)> {
    let session = desk
        .connection
        .topology
        .as_ref()?
        .sessions
        .iter()
        .find(|s| s.id == id)?;
    let phone = desk.phone.as_ref()?;
    let panes = desk
        .addressable()
        .iter()
        .filter(|r| r.session_id == id)
        .count();
    let detail = match panes {
        0 => "No panes".to_owned(),
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    let mut verbs: Verbs = vec![
        (Verb::Open, "Open".into(), &icons::WINDOW, "macwindow"),
        (Verb::NewPane, "New Pane".into(), &icons::PLUS, "plus"),
    ];
    // A search shows every match unfolded, so it offers no fold.
    let searching = !phone.query.text.trim().is_empty();
    if panes > 0 && !searching {
        let folded = phone.folded.contains(&id);
        verbs.push(if folded {
            (
                Verb::Fold,
                "Show Panes".into(),
                &icons::CHEVRON,
                "chevron.down",
            )
        } else {
            (
                Verb::Fold,
                "Fold".into(),
                &icons::CHEVRON_RIGHT,
                "chevron.up",
            )
        });
    }
    if session.key.is_some() {
        verbs.push((Verb::Rename, "Rename".into(), &icons::PENCIL, "pencil"));
        verbs.push((Verb::Close, "Close Session".into(), &icons::CLOSE, "xmark"));
    }
    Some((session.name.clone(), detail, verbs))
}

fn pane_menu(desk: &Desk, id: &ResourceId, session: &str) -> Option<(String, String, Verbs)> {
    let resource = desk.addressable().into_iter().find(|r| &r.id == id)?;
    let name = resource.presentation(desk).name;
    let mut verbs: Verbs = vec![(Verb::Open, "Open".into(), &icons::RESOURCE, "terminal")];
    // Another machine's pane is opened, not run from here.
    if id.host().is_none() {
        verbs.push((Verb::NewPane, "New Pane Here".into(), &icons::PLUS, "plus"));
        verbs.push((Verb::Close, "Close Pane".into(), &icons::CLOSE, "xmark"));
    }
    Some((name, session.to_owned(), verbs))
}

fn verb_row(verb: Verb, name: String, glyph: &'static rune::Icon, p: &Look) -> El {
    let ink = if verb == Verb::Close {
        p.status.state(State::Failed).tone.ink
    } else {
        p.roles.text.text.paint()
    };
    control(&format!("phone-menu-{verb:?}"), &name, p, move |d, _| {
        d.phone_menu_pick(verb)
    })
    .w_full()
    .gap(p.space.s2)
    .radius(nested(p))
    .child(
        text(name.clone())
            .font(p.ty.label.medium())
            .color(ink)
            .grow(1.0)
            .min_w(0.0)
            .truncate()
            .a11y_hidden(),
    )
    .child(
        el().size(p.icon.grid, p.icon.grid)
            .center()
            .child(rune::icon(glyph, p.icon.size.md).color(ink))
            .a11y_hidden(),
    )
}
