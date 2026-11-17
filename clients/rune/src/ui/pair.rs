//! Pair a phone, as Settings and onboarding's last step show it: the QR and
//! link, how long the code lasts, which network the phone uses, and the
//! devices that can connect.

use super::tokens::layout::PAIR as GEOMETRY;
use crate::ui::tokens::Voice;
use rune::{El, TextStyle, col, el, row, text};
use rune_kit::ButtonVariant;

use super::settings::{kit_button, primary, secondary};
use super::tokens::{Paint, Tokens};
use crate::desk::Desk;
use crate::pair::{self, countdown, last_seen, needs_enable};

pub(super) const DETAIL: &str =
    "Scan a code with the phux app to reach this computer's sessions from your phone.";

/// What turning remote access on means, said before the button that does it.
const ENABLE: &str = "Phones can't connect to this computer yet. Allowing it turns on paired \
     mode: only devices you pair can connect, on your tailnet address, and nothing is opened \
     to your local network or the internet. The server restarts; your sessions keep running.";

/// A kit button at the touch target: `Submit` for the step's one primary
/// action, `Outline` for the rest.
fn button(key: &'static str, label: &'static str, accent: bool, t: &Tokens) -> El {
    let variant = if accent {
        ButtonVariant::Submit
    } else {
        ButtonVariant::Outline
    };
    kit_button(key, label, variant, t)
        .min_h(t.space.touch_min)
        .self_start()
}

/// A row title: the primary copy at the label weight.
fn title(desk: &Desk, t: &Tokens) -> TextStyle {
    primary(desk, t).medium()
}

fn words(words: impl Into<String>, desk: &Desk, t: &Tokens) -> El {
    text(words.into())
        .font(secondary(desk, t))
        .color(t.roles.text.dim.paint())
        .wrap()
}

/// The code on its plate: white by requirement, so any camera reads it in
/// either mode.
fn qr(desk: &Desk, t: &Tokens) -> El {
    let frame = el()
        .key("pair-qr")
        .size(GEOMETRY.qr, GEOMETRY.qr)
        .radius(t.radius.sm)
        .clip_rounded(t.radius.sm)
        .fill(rune::Color::WHITE)
        .border(t.space.hairline, t.roles.border.base.paint())
        .role("image")
        .label("Pairing QR code");
    match &desk.settings_ui.pairing.qr {
        Some(image) => frame.pixels(image.pixels()),
        None => frame.center().child(
            text("Copy the link instead")
                .font(t.ty.caption)
                .color(rune::Color::BLACK)
                .a11y_hidden(),
        ),
    }
}

/// The code and what to do with it, or how to get one.
fn code(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let pairing = &desk.settings_ui.pairing;
    if pairing.minting {
        return words("Making a code…", desk, t).key("pair-minting");
    }
    match &pairing.code {
        None => button("pair-show", "Show code", false, t).on_press(Desk::new_pair_code),
        Some(Err(error)) if needs_enable(error) => col()
            .w_full()
            .gap(s.s3)
            .child(words(ENABLE, desk, t))
            .child(
                button("pair-allow", "Allow phones to connect", true, t)
                    .on_press(Desk::allow_phones),
            ),
        Some(Err(error)) => col()
            .w_full()
            .gap(s.s3)
            .child(words(error.clone(), desk, t).key("pair-error"))
            .child(button("pair-retry", "Try again", false, t).on_press(Desk::new_pair_code)),
        Some(Ok(code)) => {
            let now = pair::now();
            let left = code.expires_at.map(|at| countdown(at - now));
            let expired = matches!(left, Some(None));
            let status = match left {
                Some(Some(left)) if code.once => format!("Expires in {left}. Works once."),
                Some(Some(left)) => format!("Expires in {left}."),
                Some(None) => "This code expired.".to_owned(),
                None => "Works until you make a new one or revoke it.".to_owned(),
            };
            let mut buttons = row().w_full().gap(s.s2).flex_wrap();
            if !expired {
                let copy = if pairing.copied {
                    "Copied"
                } else {
                    "Copy link"
                };
                buttons = buttons.child(
                    button("pair-copy", copy, false, t)
                        .on_press_tx(|d: &mut Desk, tx| d.copy_pair_link(tx)),
                );
            }
            buttons = buttons
                .child(button("pair-new", "New code", expired, t).on_press(Desk::new_pair_code));
            let side = col()
                .grow(1.0)
                .min_w(GEOMETRY.side_min)
                .gap(s.s2)
                .child(
                    text(status)
                        .key("pair-expiry")
                        .font(title(desk, t))
                        .color(t.roles.text.text.paint())
                        .wrap(),
                )
                .child(words(code.network.words(), desk, t).key("pair-network"))
                .child(words(
                    "The link and code are secrets: share them only with your own phone.",
                    desk,
                    t,
                ))
                .child(buttons);
            row()
                .w_full()
                .gap(s.s4)
                .flex_wrap()
                .items_start()
                .children((!expired).then(|| qr(desk, t)))
                .child(side)
        }
    }
}

fn devices(desk: &Desk, t: &Tokens) -> Option<El> {
    let s = &t.space;
    let listed = desk.settings_ui.pairing.devices.as_ref()?;
    let mut list = col().key("pair-devices").w_full().gap(s.s1).child(
        text("Paired devices")
            .font(secondary(desk, t).semibold())
            .color(t.roles.text.dim.paint())
            .role("heading")
            .level(3),
    );
    let now = pair::now();
    let devices: Vec<_> = match listed {
        Err(error) => return Some(list.child(words(error.clone(), desk, t))),
        // An older CLI lists the code on screen too, until a phone uses it.
        Ok(devices) => devices
            .iter()
            .filter(|d| {
                let shown = desk.settings_ui.pairing.live_code(now);
                d.last_seen.is_some() || shown.is_none_or(|code| code.credential != d.id)
            })
            .collect(),
    };
    if devices.is_empty() {
        return Some(list.child(words("No devices yet.", desk, t)));
    }
    for (at, device) in devices.into_iter().enumerate() {
        let id = device.id.clone();
        let name = device.name();
        list = list.child(
            row()
                .key(format!("pair-device-{at}"))
                .w_full()
                .min_h(s.s12)
                .gap(s.s3)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(s.s0_5)
                        .child(
                            text(name.clone())
                                .font(title(desk, t))
                                .color(t.roles.text.text.paint()),
                        )
                        .child(words(last_seen(device.last_seen, now), desk, t)),
                )
                .child(
                    button("pair-revoke", "Revoke", false, t)
                        .key(format!("pair-revoke-{at}"))
                        .label(format!("Revoke {name}"))
                        .on_press(move |d: &mut Desk| d.revoke_device(id.clone())),
                ),
        );
    }
    Some(list.child(words(
        "Revoking a device stops it connecting again; it can pair again with a new code.",
        desk,
        t,
    )))
}

/// The whole panel. Onboarding leaves the device list to Settings.
pub(super) fn panel(desk: &Desk, t: &Tokens, with_devices: bool) -> El {
    let s = &t.space;
    col()
        .key("pair-phone")
        .w_full()
        .padding(s.s4)
        .gap(s.s3)
        .child(
            col()
                .w_full()
                .gap(s.s0_5)
                .child(
                    text("Pair a phone")
                        .font(title(desk, t))
                        .color(t.roles.text.text.paint())
                        .role("heading")
                        .level(3),
                )
                .child(words(DETAIL, desk, t)),
        )
        .child(code(desk, t))
        .children(with_devices.then(|| devices(desk, t)).flatten())
}
