//! The Sessions page's floating New session dock: native Liquid Glass on
//! the iOS host, Rune's frosted glass elsewhere.

use rune::{Color, El, text};

use super::{Desk, Look, control};
use crate::ui::icons;
use crate::ui::tokens::{GlassSurface, Paint, Tier, Voice};

/// New session, or Open or create `name` while a search names one; inert
/// until a connection can run it. At large text sizes it is an opaque
/// action at the end of the list, not glass floating over its text.
pub(super) fn footer(desk: &Desk, name: &str, ink: Color, large_text: bool, p: &Look) -> El {
    let create = if name.is_empty() {
        "New session".to_owned()
    } else {
        format!("Open or create “{name}”")
    };
    let title = if name.is_empty() {
        "New session"
    } else {
        "Open or create session"
    };
    let footer = control("phone-session-new", &create, p, |d, _| {
        d.phone_create_session()
    })
    .w_full()
    .radius(p.radius.full);
    let footer = if large_text {
        labelled(footer, title, ink, p).fill(p.roles.layer.raised.paint())
    } else if super::super::native_glass(desk, p) {
        let control = crate::mobile::Control {
            key: "phone-session-new".to_owned(),
            label: create,
            title: Some(title.to_owned()),
            subtitle: None,
            leading: false,
            symbol: "plus",
            enabled: desk.connection.client.is_some(),
            dark: p.dark(),
            ink,
            badge: None,
        };
        super::super::hosted(desk, footer, control)
    } else {
        frosted(labelled(footer, title, ink, p), p)
    };
    if desk.connection.client.is_some() {
        footer
    } else {
        footer.inert()
    }
}

/// Rune's own plus and title on the control, wrapping at large text.
fn labelled(footer: El, title: &str, ink: Color, p: &Look) -> El {
    footer
        .gap(p.space.s2)
        .center()
        .child(
            rune::icon(&icons::PLUS, p.icon.size.lg)
                .color(ink)
                .a11y_hidden(),
        )
        .child(
            text(title)
                .key("phone-session-new-label")
                .font(p.ty.label.medium())
                .color(ink)
                .wrap()
                .min_w(0.0)
                .shrink()
                .a11y_hidden(),
        )
}

/// The session dock: glass over the scrolling list, the phone's one
/// backdrop blur. Opaque under increased contrast, Reduce
/// Transparency and forced colours. The native host draws it as the
/// system's Liquid Glass instead ([`footer`]).
fn frosted(e: El, p: &Look) -> El {
    let e = p.glass_panel(
        e,
        GlassSurface::PhoneBar,
        p.glass_tint_layer(),
        p.roles.layer.raised,
    );
    p.elevate(e.continuous_corners(), Tier::Floating)
}
