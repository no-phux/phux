//! The phone form: a live terminal with touch chrome outside its viewport:
//! a top bar, navigation toolbar, tabs, accessory bar and keyboard. What
//! Rune can't do on a phone yet (safe areas, the system keyboard) is drawn
//! here instead; clients/rune/MOBILE_GAPS.md lists each piece and what replaces it.
//!
//! Every control is a touch target of `space.touch_min` on an opaque
//! surface: the phone spends its one backdrop blur on the session dock,
//! so nothing here fakes glass. On the native iOS host the floating controls
//! (the top bar's back and more, and the session dock) are instead
//! UIKit's own Liquid Glass ([`hosted`]).

use std::ops::Deref;
use std::rc::Rc;

use phux_design::State;
use rune::{Color, Cursor, Cx, El, col, el};

use super::tokens::phone::{self as form, Scale};
use super::tokens::{Paint, Tier, Tokens, Type};
use super::{badge, icons, news, overlay, panes, settings, spatial, status};
use crate::activity::Mark;
use crate::desk::Desk;
use crate::desk::phone::Phone;

mod accessory;
mod bars;
mod composer;
mod find;
mod keyboard;
pub(super) mod motion;
pub(super) mod navigation;
pub(super) mod sheet;
use accessory::accessory;
use bars::{tab_pills, top_bar};
use keyboard::keyboard;

/// What the phone styles with: tokens for a ground and the
/// phone's own type roles at the reported text size. Reads as its
/// [`Tokens`].
pub(super) struct Look {
    t: Rc<Tokens>,
    /// The phone's type roles, not the desktop's `Tokens::ty`.
    pub(super) ty: Type,
}

impl Look {
    fn new(t: Rc<Tokens>) -> Self {
        Self {
            ty: form::ty(t.type_scale),
            t,
        }
    }

    /// A round touch target's diameter.
    pub(super) fn round(&self) -> f32 {
        self.space.touch_min
    }
}

impl Deref for Look {
    type Target = Tokens;
    fn deref(&self) -> &Tokens {
        &self.t
    }
}

/// The top bar's capsule and the gap under it: where a banner starts.
pub(super) fn top_bar_height() -> f32 {
    let s = phux_design::tokens::SPACE;
    s.touch_min + s.s1 + s.s2
}

pub fn view(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    form::observe(cx);
    // This run notes the native controls it lays out afresh.
    if let Some(native) = &desk.native {
        native.controls.borrow_mut().clear();
    }
    let p = page_look(desk);
    let fallback = Phone::default();
    let phone = desk.phone.as_ref().unwrap_or(&fallback);
    let keyboard_visible = desk.phone_keyboard_visible();
    // The controls beside the terminal, which may not have taken a newly
    // chosen theme yet: their ink contrasts with the ground actually drawn.
    let drawn = ground_look(desk);
    let controls = if desk.settings_open || desk.phone_page().is_some() {
        &p
    } else {
        &drawn
    };
    // The window wired the system appearance's tokens; the phone's surfaces
    // take the theme's or the ground's mode, so its ink and ring follow.
    cx.foreground(p.roles.text.text.paint());
    cx.caption_color(p.roles.text.text.paint());
    let ring = p.space.focus_ring;
    cx.focus_ring(ring.width, ring.offset, p.roles.border.focus.paint());
    let stage = phone_stage(desk, &p, cx);
    let top = top_inset(desk, &p, cx);
    let screen = col()
        .key("phone-screen")
        .w_full()
        .h_full()
        .pt(top)
        .children((!desk.settings_open).then(|| top_bar(desk, controls)))
        .child(stage)
        .children(
            (!desk.settings_open)
                .then(|| tab_pills(desk, controls))
                .flatten(),
        )
        .children(composer::view(desk, controls))
        // Find docks over the key bar, out of the terminal it searches.
        .children((!desk.settings_open).then(|| find::view(desk, controls)).flatten());
    // Sessions and Actions are lists, not a terminal: no key bar, and their
    // search keyboard's Return is its Done. Settings keeps the bar only for
    // its search's Done.
    let bar = if desk.phone_page().is_some() {
        None
    } else {
        (!desk.settings_open || keyboard_visible).then(|| accessory(desk, phone, controls))
    };
    let fill = edge_fill(desk, &p);
    // Both keyboards sit after every sheet and kit overlay, outside their
    // measured viewport. The compatible Rune pin (DEVELOP.md) treats post-modal
    // controls as part of the modal: keys remain live while the scrim still
    // blocks the terminal and navigation behind it.
    let content = overlays(
        above_leaving(
            el().h(0.0).grow(1.0).w_full().min_h(0.0).child(screen),
            desk,
        ),
        desk,
        &p,
        top,
        cx,
    );
    let root = col().key("phone").w_full().h_full();
    super::glassed(root, fill, desk.glass(), &p)
        .child(content)
        .children(bar)
        .children(
            desk.native
                .is_none()
                .then(|| keyboard(phone, keyboard_visible, controls)),
        )
}

/// The terminal's own ground runs under everything, as on a phone. Before
/// a terminal is on screen it is the theme's, or the black a terminal
/// starts on: never the system's light page behind a dark launch.
pub(super) fn terminal_ground(desk: &Desk) -> Color {
    let frame = desk
        .connection
        .client
        .as_ref()
        .zip(desk.connection.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view?));
    frame.map_or_else(
        || {
            let theme = desk.theme();
            theme.map_or(Color::BLACK, |theme| crate::theme::color(theme.background))
        },
        |frame| crate::grid::background(&frame),
    )
}

/// A phone shows one surface at a time, so its sheets and pages take the
/// chosen theme's tokens, or with none the terminal ground's mode, never
/// the system appearance: a light-mode phone would otherwise open white
/// sheets over a dark terminal. Pages are free of the grid's text cap.
pub(super) fn page_look(desk: &Desk) -> Look {
    let theme = desk.theme();
    let dark = theme.map_or_else(|| Tokens::dark_ground(terminal_ground(desk)), |t| t.dark);
    Look::new(Tokens::for_phone(desk, dark, theme, Scale::Free))
}

/// The controls beside the terminal: their mode is the ground's, so they
/// read on the terminal they sit beside; a theme of that mode colours
/// them. Capped beside the grid.
pub(super) fn ground_look(desk: &Desk) -> Look {
    let dark = Tokens::dark_ground(terminal_ground(desk));
    Look::new(Tokens::for_phone(
        desk,
        dark,
        desk.theme(),
        Scale::GridCapped,
    ))
}

/// The screen's content, painted over the key bar while Settings comes or
/// goes: the bar it replaces fades where it stood, under the page sliding
/// over it, never over it. They never overlap otherwise.
fn above_leaving(content: El, desk: &Desk) -> El {
    if desk.settings_settling() {
        content.paint_order(1)
    } else {
        content
    }
}

fn edge_fill(desk: &Desk, p: &Look) -> Color {
    if desk.settings_open {
        p.roles.layer.sidebar.paint()
    } else if desk.phone_page().is_some() {
        // The layer the drawer's glass is tinted with, which it reads as
        // over the terminal it blurs; its ground where it is opaque.
        let glass = p.glass(super::tokens::GlassSurface::PhoneBar, p.glass_tint_layer());
        if glass.is_some() {
            p.glass_tint_layer().paint()
        } else {
            p.roles.layer.ground.paint()
        }
    } else {
        terminal_ground(desk)
    }
}

/// The phone's edge colour, for a native host to continue under the status
/// bar and home indicator it keeps outside the surface.
pub(crate) fn edge_color(desk: &Desk) -> Color {
    edge_fill(desk, &page_look(desk))
}

/// Sheets, then the kit's overlays (menus, selects, toasts) this run built,
/// all inside the content box, so the phone's keyboards stay after them
///. The window root's kit layer is then empty on the phone.
fn overlays(root: El, desk: &Desk, ink: &Tokens, top: f32, cx: &mut Cx<Desk>) -> El {
    let root = root.children(status::banner(desk, ink));
    let root = super::floating_surfaces(root, desk, ink, top, cx);
    root.children(super::KIT_LAYER.with_borrow_mut(std::mem::take))
}

fn top_inset(desk: &Desk, p: &Look, cx: &Cx<Desk>) -> f32 {
    // Native UIKit already constrains the surface to its safe area. On a
    // desktop, traffic lights occupy the simulated status bar's place.
    if desk.native.is_some() {
        0.0
    } else if cx.titlebar_inset() > 0.0 {
        p.space.titlebar_h
    } else {
        p.space.s3
    }
}

/// Modals precede the drawn keyboard so its keys belong to their fields.
/// While Settings comes or goes, the stage lands where the top bar puts
/// it at once, so only the page moves.
fn phone_stage(desk: &Desk, p: &Look, cx: &mut Cx<Desk>) -> El {
    let stage = col().key("phone-stage");
    let stage = if desk.settings_settling() {
        // Over the top bar it replaces, too, as that fades where it stood.
        stage.motion(rune::Spring::NONE).paint_order(1)
    } else {
        stage
    };
    stage
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .min_w(0.0)
        .child(if desk.settings_open {
            settings::view(desk, p)
        } else {
            panes::view(desk, p, cx)
        })
        .children(status::confirm_bar(desk, p))
        .children(overlay::toast(desk, p))
}

/// An opaque control surface over the terminal's ground: the raised layer
/// on the raised tier, its ring the edge. Only the session dock is
/// glass on a phone.
pub(super) fn surface(e: El, p: &Tokens) -> El {
    p.elevate(
        e.continuous_corners().fill(p.roles.layer.raised.paint()),
        Tier::Raised,
    )
}

/// A touch control's hover (an iPad pointer) and press washes, and the
/// pointer.
pub(super) fn touchable(e: El, p: &Tokens) -> El {
    p.kit.interactive(e).cursor(Cursor::Pointer)
}

/// Whether the floating controls are the system's Liquid Glass: on the
/// native host, wherever glass may draw at all (§1.9). Under increased
/// contrast, Reduce Transparency or forced colours Rune paints them opaque,
/// as on every other host. The phone has no Glass slider (`Desk::glass`
/// is the desktop's), so the setting is the system's.
pub(super) fn native_glass(desk: &Desk, p: &Tokens) -> bool {
    desk.native.is_some() && p.glass_allowed()
}

/// `placeholder`, an empty labelled box with the control's key and size,
/// noted for the native host to draw as `control` (see
/// [`crate::mobile::Control`]). The caller keeps its press and state on
/// the box, so a native tap and Rune's accessibility press run the same
/// handler. Rune's accessibility node stays: it is not iOS VoiceOver's
/// (the UIKit control is), so nothing is announced twice.
pub(super) fn hosted(desk: &Desk, placeholder: El, control: crate::mobile::Control) -> El {
    if let Some(native) = &desk.native {
        native.controls.borrow_mut().push(control);
    }
    placeholder
}

/// `page`, while it is the page a swipe would take ([`Desk::swipe_away`]),
/// bound to where the finger has it: it follows one to one without a
/// rebuild or a layout, and a let-go short of leaving springs it home on
/// the drawer spring. A drawer or a level fades as it goes; a sheet slides
/// whole, as a system sheet does, so a glass sheet never blurs inside a
/// fading layer. Never a page under the top one.
pub(in crate::ui) fn swipeable(
    desk: &Desk,
    page: El,
    away: crate::desk::phone::dismiss::Away,
    p: &Tokens,
) -> El {
    use crate::desk::phone::dismiss::Way;
    let Some(phone) = desk.phone.as_ref().filter(|_| desk.swipes(away)) else {
        return page;
    };
    let spring = p.motion.drawer.enter;
    let page = page.live_offset(phone.dismiss.shift.smooth(spring));
    match away.way() {
        Way::Down => page,
        Way::Left | Way::Back => page.live_opacity(phone.dismiss.shown.smooth(spring)),
    }
}

/// A list a downward swipe defers to until it is at its top: it reports
/// its offset live, so scrolling it runs no view.
pub(in crate::ui) fn swipe_list(
    desk: &Desk,
    list: El,
    away: crate::desk::phone::dismiss::Away,
) -> El {
    match desk.phone.as_ref() {
        Some(phone) => list.on_scroll_offset(phone.dismiss.follow_list(away)),
        None => list,
    }
}

/// A floating round symbol button with an optional dot (`badge`): native
/// Liquid Glass where [`native_glass`], else [`round`] with `badge` drawn
/// on its upper trailing shoulder.
fn floating_round(
    desk: &Desk,
    key: &str,
    name: &str,
    icon: &'static rune::Icon,
    symbol: &'static str,
    badge: Option<Color>,
    p: &Tokens,
) -> El {
    let ink = p.roles.text.text.paint();
    if !native_glass(desk, p) {
        let dot = (p.space.touch_min - p.space.s4, p.space.s2);
        return round(key, name, glyph(icon, p.icon.size.lg, ink), p)
            .children(badge.map(|tone| news::dot(p, dot, p.space.s2, tone)));
    }
    let placeholder = el()
        .key(key)
        .size(p.space.touch_min, p.space.touch_min)
        .radius(p.radius.full)
        .label(name.to_owned());
    let control = crate::mobile::Control {
        key: key.to_owned(),
        label: name.to_owned(),
        title: None,
        symbol,
        subtitle: None,
        leading: false,

        enabled: true,
        dark: p.dark(),
        ink,
        badge,
        painted: false,
    };
    hosted(desk, placeholder, control)
}

/// The close every phone sheet and page ends its header with, on its
/// trailing edge: one round ✕, the system's Liquid Glass on the native
/// host as the top bar's buttons are. `name` says what it closes; the
/// caller gives it its press.
pub(in crate::ui) fn close_button(desk: &Desk, key: &str, name: &str, p: &Tokens) -> El {
    floating_round(desk, key, name, &icons::CLOSE, "xmark", None, p)
}

/// A level's way back, leading its header where there is a level to go
/// back to: the round ‹ beside the close, made the same way.
pub(in crate::ui) fn back_button(desk: &Desk, key: &str, name: &str, p: &Tokens) -> El {
    floating_round(
        desk,
        key,
        name,
        &icons::CHEVRON_LEFT,
        "chevron.left",
        None,
        p,
    )
}

/// A round control around `content`: the 44-point touch target.
fn round(key: &str, name: &str, content: El, p: &Tokens) -> El {
    let circle = el()
        .key(key)
        .size(p.space.touch_min, p.space.touch_min)
        .radius(p.radius.full)
        .center();
    touchable(surface(circle, p), p)
        .label(name.to_owned())
        .child(content)
}

pub(super) fn glyph(icon: &'static rune::Icon, size: f32, color: Color) -> El {
    rune::icon(icon, size).color(color)
}

/// What the phone shows of a terminal's mark: the status ladder's state,
/// none for a plain shell. The bell is its own tone and mark.
pub(super) fn state_of(mark: Mark) -> Option<State> {
    (mark != Mark::None).then(|| super::sidebar::state_of(mark))
}

/// `mark`'s ink, or quiet ink for a plain shell.
fn mark_ink(mark: Mark, p: &Look) -> Color {
    match (mark, state_of(mark)) {
        (Mark::Bell, _) => p.status.tone(phux_design::Tone::Bell).ink,
        (_, Some(state)) => p.status.state(state).tone.ink,
        (_, None) => p.roles.text.dim.paint(),
    }
}

#[cfg(test)]
mod tests;
