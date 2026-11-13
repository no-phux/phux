//! Pair a phone, as Settings and onboarding's last step show it: the QR and
//! link, how long the code lasts, which network the phone uses, and the
//! devices that can connect.

use rune::{El, TextStyle, col, el, row, text};

use super::settings::{primary, secondary};
use super::{CAPTION, Ink, pressable};
use crate::desk::Desk;
use crate::pair::{self, access_left, countdown, needs_enable};

const TITLE: TextStyle = TextStyle::new(13.0, 500.0);
const COPY: TextStyle = TextStyle::new(12.0, 450.0).lh(16.0);
const QR_SIDE: f32 = 200.0;

pub(super) const DETAIL: &str =
    "Scan a code with the phux app to reach this computer's sessions from your phone.";

/// What turning remote access on means, said before the button that does it.
const ENABLE: &str = "Phones can't connect to this computer yet. Allowing it turns on paired \
     mode: only devices you pair can connect, on your tailnet address, and nothing is opened \
     to your local network or the internet. The server restarts; your sessions keep running.";

fn button(key: &'static str, label: &'static str, accent: bool, ink: &Ink, desk: &Desk) -> El {
    let el = pressable(key, ink)
        .focusable(true)
        .role("button")
        .label(label)
        .min_h(44.0)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .center()
        .self_start();
    let (el, color) = if accent {
        (el.fill(ink.accent), ink.on_accent)
    } else {
        (el.border(1.0, ink.separator), ink.text)
    };
    el.child(
        text(label)
            .font(primary(desk, CAPTION))
            .color(color)
            .a11y_hidden(),
    )
}

fn words(words: impl Into<String>, desk: &Desk, ink: &Ink) -> El {
    text(words.into())
        .font(secondary(desk, COPY))
        .color(ink.dim)
        .wrap()
}

fn qr(desk: &Desk, ink: &Ink) -> El {
    let frame = el()
        .key("pair-qr")
        .size(QR_SIDE, QR_SIDE)
        .radius(8.0)
        .clip_rounded(8.0)
        .fill(rune::Color::WHITE)
        .border(1.0, ink.separator)
        .role("image")
        .label("Pairing QR code");
    match &desk.settings_ui.pairing.qr {
        Some(image) => frame.pixels(image.pixels()),
        None => frame.center().child(
            text("Copy the link instead")
                .font(CAPTION)
                .color(rune::Color::BLACK)
                .a11y_hidden(),
        ),
    }
}

/// The code and what to do with it, or how to get one.
fn code(desk: &Desk, ink: &Ink) -> El {
    let pairing = &desk.settings_ui.pairing;
    if pairing.minting {
        return words("Making a code…", desk, ink).key("pair-minting");
    }
    match &pairing.code {
        None => button("pair-show", "Show code", false, ink, desk).on_press(Desk::new_pair_code),
        Some(Err(error)) if needs_enable(error) => col()
            .w_full()
            .gap(10.0)
            .child(words(ENABLE, desk, ink))
            .child(
                button("pair-allow", "Allow phones to connect", true, ink, desk)
                    .on_press(Desk::allow_phones),
            ),
        Some(Err(error)) => col()
            .w_full()
            .gap(10.0)
            .child(words(error.clone(), desk, ink).key("pair-error"))
            .child(
                button("pair-retry", "Try again", false, ink, desk).on_press(Desk::new_pair_code),
            ),
        Some(Ok(code)) => {
            let now = pair::now();
            let left = code.expires_at.map(|at| countdown(at - now));
            let expired = matches!(left, Some(None));
            let status = match left {
                Some(Some(left)) => format!("Expires in {left}. Works once."),
                Some(None) => "This code expired.".to_owned(),
                None => "Works until you make a new one or revoke it.".to_owned(),
            };
            let mut buttons = row().w_full().gap(8.0).flex_wrap();
            if !expired {
                buttons = buttons.child(
                    button(
                        "pair-copy",
                        if pairing.copied {
                            "Copied"
                        } else {
                            "Copy link"
                        },
                        false,
                        ink,
                        desk,
                    )
                    .on_press_tx(|d: &mut Desk, tx| d.copy_pair_link(tx)),
                );
            }
            buttons = buttons.child(
                button("pair-new", "New code", expired, ink, desk).on_press(Desk::new_pair_code),
            );
            let side = col()
                .grow(1.0)
                .min_w(180.0)
                .gap(8.0)
                .child(
                    text(status)
                        .key("pair-expiry")
                        .font(primary(desk, TITLE))
                        .color(ink.text)
                        .wrap(),
                )
                .child(words(code.network.words(), desk, ink).key("pair-network"))
                .child(words(
                    "The link and code are secrets: share them only with your own phone.",
                    desk,
                    ink,
                ))
                .child(buttons);
            row()
                .w_full()
                .gap(16.0)
                .flex_wrap()
                .items_start()
                .children((!expired).then(|| qr(desk, ink)))
                .child(side)
        }
    }
}

fn devices(desk: &Desk, ink: &Ink) -> Option<El> {
    let listed = desk.settings_ui.pairing.devices.as_ref()?;
    let mut list = col().key("pair-devices").w_full().gap(4.0).child(
        text("Paired devices")
            .font(secondary(desk, TextStyle::new(12.0, 600.0)))
            .color(ink.dim)
            .role("heading")
            .level(3),
    );
    let devices = match listed {
        Err(error) => return Some(list.child(words(error.clone(), desk, ink))),
        Ok(devices) if devices.is_empty() => {
            return Some(list.child(words("No devices yet.", desk, ink)));
        }
        Ok(devices) => devices,
    };
    let now = pair::now();
    for (at, device) in devices.iter().enumerate() {
        let id = device.id.clone();
        let name = device.name();
        list = list.child(
            row()
                .key(format!("pair-device-{at}"))
                .w_full()
                .min_h(48.0)
                .gap(12.0)
                .items_center()
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(2.0)
                        .child(
                            text(name.clone())
                                .font(primary(desk, TITLE))
                                .color(ink.text),
                        )
                        .child(words(access_left(device.expires_at, now), desk, ink)),
                )
                .child(
                    button("pair-revoke", "Revoke", false, ink, desk)
                        .key(format!("pair-revoke-{at}"))
                        .label(format!("Revoke {name}"))
                        .on_press(move |d: &mut Desk| d.revoke_device(id.clone())),
                ),
        );
    }
    Some(list.child(words(
        "Phones and computers enrolled with this one. Revoking a device disconnects it now; \
         it can pair again with a new code.",
        desk,
        ink,
    )))
}

/// The whole panel. Onboarding leaves the device list to Settings.
pub(super) fn panel(desk: &Desk, ink: &Ink, with_devices: bool) -> El {
    col()
        .key("pair-phone")
        .w_full()
        .padding(16.0)
        .gap(12.0)
        .child(
            col()
                .w_full()
                .gap(3.0)
                .child(
                    text("Pair a phone")
                        .font(primary(desk, TITLE))
                        .color(ink.text)
                        .role("heading")
                        .level(3),
                )
                .child(words(DETAIL, desk, ink)),
        )
        .child(code(desk, ink))
        .children(with_devices.then(|| devices(desk, ink)).flatten())
}
