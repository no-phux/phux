//! The bottom action a navigation page keeps under the thumb: Sessions'
//! New session, Terminal actions' Close. Native Liquid Glass on the iOS
//! host and an opaque raised capsule elsewhere; at large text sizes a
//! wrapping card that scrolls with the list.

use rune::{Color, El, text};

use super::{Desk, Look, control, grid};
use crate::ui::icons;
use crate::ui::tokens::{Paint, Voice};

/// One dock action: what it says and shows, and what it does.
struct Dock {
    key: &'static str,
    /// The accessible name: the whole action.
    label: String,
    /// The words on the capsule.
    title: &'static str,
    icon: &'static rune::Icon,
    /// The SF Symbol the native glass control draws.
    symbol: &'static str,
    enabled: bool,
}

/// New session, or Open or create `name` while a search names one; inert
/// until a connection can run it. At large text sizes it scrolls with the list.
pub(super) fn footer(desk: &Desk, name: &str, ink: Color, large_text: bool, p: &Look) -> El {
    let (label, title) = if name.is_empty() {
        ("New session".to_owned(), "New session")
    } else {
        (format!("Open or create “{name}”"), "Open or create session")
    };
    let dock = Dock {
        key: "phone-session-new",
        label,
        title,
        icon: &icons::PLUS,
        symbol: "plus",
        // Nothing can run it until a connection can.
        enabled: desk.connection.client.is_some(),
    };
    capsule(desk, dock, ink, large_text, p, |d, _| {
        d.phone_create_session()
    })
}

/// Back to the terminal, under the thumb on a page whose Done is at the
/// top: the same dismissal as Done and a swipe down, whose way its chevron
/// points. It names where it goes, never "Close" beside Close Terminal.
pub(super) fn close(desk: &Desk, p: &Look) -> El {
    let dock = Dock {
        key: "phone-navigation-dock-close",
        label: "Back to terminal".to_owned(),
        title: "Back to terminal",
        icon: &icons::CHEVRON,
        symbol: "chevron.down",
        enabled: true,
    };
    let large_text = p.type_scale >= crate::ui::tokens::layout::PHONE.stacked_title_scale;
    let ink = p.roles.text.text.paint();
    capsule(desk, dock, ink, large_text, p, |d, tx| {
        d.close_phone_navigation();
        tx.blur();
    })
}

fn capsule(
    desk: &Desk,
    dock: Dock,
    ink: Color,
    large_text: bool,
    p: &Look,
    action: impl Fn(&mut Desk, &mut rune::Tx) + Clone + 'static,
) -> El {
    // A disabled action's words take the faint ink as well as being inert.
    let words = if dock.enabled {
        ink
    } else {
        p.roles.text.faint.paint()
    };
    let capsule = control(dock.key, &dock.label, p, action)
        .w_full()
        .radius(p.radius.full);
    let capsule = if large_text {
        labelled(capsule, &dock, words, p)
            .radius(p.radius.lg)
            .fill(grid::card_fill(p))
    } else if super::super::native_glass(desk, p) {
        let control = crate::mobile::Control {
            key: dock.key.to_owned(),
            label: dock.label.clone(),
            title: Some(dock.title.to_owned()),
            subtitle: None,
            leading: false,
            symbol: dock.symbol,
            enabled: dock.enabled,
            dark: p.dark(),
            ink,
            badge: None,
            painted: false,
        };
        super::super::hosted(desk, capsule, control)
    } else {
        labelled(capsule, &dock, words, p)
            .continuous_corners()
            .fill(grid::card_fill(p))
    };
    if dock.enabled {
        capsule
    } else {
        capsule.inert()
    }
}

/// Rune's own glyph and title on the capsule, wrapping at large text.
fn labelled(capsule: El, dock: &Dock, ink: Color, p: &Look) -> El {
    capsule
        .gap(p.space.s2)
        .center()
        .child(
            rune::icon(dock.icon, p.icon.size.lg)
                .color(ink)
                .a11y_hidden(),
        )
        .child(
            text(dock.title)
                .key(format!("{}-label", dock.key))
                .font(p.ty.label.medium())
                .color(ink)
                .wrap()
                .min_w(0.0)
                .shrink()
                .a11y_hidden(),
        )
}
