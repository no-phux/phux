//! Updates on screen: a corner card that never takes the keyboard, the
//! status-line pill that reopens it, and the section in settings.

use rune::{Color, El, Transition, col, el, row, text};

use super::settings::{section, toggle};
use super::{CAPTION, Ink, LABEL, LABEL_STRONG};
use crate::desk::Desk;
use crate::desk::updates::{Action, Updater, Work};
use crate::updates::VERSION;

const CARD_W: f32 = 320.0;

fn act_button(action: Action, key: &'static str, ink: &Ink) -> El {
    el().key(key)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .fill(ink.accent)
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(|d: &mut Desk, tx| d.update_act(tx))
        .child(text(action.label()).font(CAPTION).color(Color::WHITE))
}

fn quiet_button(label: &'static str, key: &'static str, press: fn(&mut Desk), ink: &Ink) -> El {
    el().key(key)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .border(1.0, ink.separator)
        .hover(ink.hover)
        .cursor(rune::Cursor::Pointer)
        .on_press(press)
        .child(text(label).font(CAPTION).color(ink.text))
}

/// Real transfer progress only; verifying has no percentage to show.
fn progress(percent: u8, width: f32, ink: &Ink) -> El {
    el().w(width).h(3.0).radius(1.5).fill(ink.separator).child(
        el().w(width * f32::from(percent) / 100.0)
            .h(3.0)
            .radius(1.5)
            .fill(ink.accent),
    )
}

fn headline(updater: &Updater) -> String {
    let incoming = updater.staged.as_ref().or(updater
        .release
        .as_ref()
        .filter(|_| updater.app_due())
        .map(|release| &release.version));
    match (updater.work, updater.action(), incoming) {
        (Work::Downloading(percent), ..) => format!("Downloading… {percent}%"),
        (Work::Verifying, ..) => "Verifying…".to_owned(),
        (Work::Server, ..) => "Updating phux and handing the server off…".to_owned(),
        (Work::Checking, ..) => "Checking…".to_owned(),
        (_, Some(Action::UpdateServer { .. }), _) => "A phux update is ready".to_owned(),
        (_, _, Some(version)) if updater.staged.is_some() => {
            format!("Phux Rune {version} installs on restart")
        }
        (_, _, Some(version)) => format!("Phux Rune {version} is available"),
        _ if updater.release.is_some() => "You're up to date".to_owned(),
        _ => "Not checked yet".to_owned(),
    }
}

/// What the update means for the sessions and the server.
fn detail(updater: &Updater) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(cli) = updater.cli.as_ref().filter(|cli| cli.available) {
        lines.push(match &cli.native_command {
            Some(command) if !cli.mutable => {
                format!("phux {} is out. Update it with: {command}", cli.target)
            }
            _ => format!("phux {} → {}", cli.current, cli.target),
        });
    }
    if let Some((now, incoming)) = updater.protocol_warning() {
        lines.push(format!(
            "This changes the protocol from {now} to {incoming}; the server needs updating too."
        ));
    }
    lines.extend(updater.message.clone());
    lines
}

fn lines(updater: &Updater, ink: &Ink) -> El {
    let mut list = col().w_full().gap(2.0);
    for line in detail(updater) {
        list = list.child(text(line).font(CAPTION).color(ink.dim).wrap());
    }
    list
}

pub fn card(desk: &Desk, ink: &Ink, top: f32) -> Option<El> {
    let updater = &desk.updater;
    if !updater.card_open() {
        return None;
    }
    let inner = CARD_W - 28.0;
    let percent = match updater.work {
        Work::Downloading(percent) => Some(percent),
        _ => None,
    };
    let body = col()
        .key("update-card")
        .w(CARD_W)
        .padding(14.0)
        .gap(8.0)
        .radius(12.0)
        .fill(ink.raised)
        .border(1.0, ink.separator)
        .shadow(10.0, 32.0, Color::BLACK.alpha(0.35))
        .transition(Transition::opacity())
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    text(headline(updater))
                        .font(LABEL_STRONG)
                        .color(ink.text)
                        .wrap()
                        .grow(1.0),
                )
                .child(
                    el().key("update-dismiss")
                        .cursor(rune::Cursor::Pointer)
                        .on_press(|d: &mut Desk| d.updater.dismiss())
                        .child(text("✕").font(CAPTION).color(ink.dim)),
                ),
        )
        .child(lines(updater, ink))
        .children(percent.map(|percent| progress(percent, inner, ink)))
        .children(
            updater
                .action()
                .map(|action| row().child(act_button(action, "update-act", ink))),
        );
    Some(
        row()
            .key("update-corner")
            .absolute(0.0, top + 8.0)
            .w_full()
            .pr(14.0)
            .justify_end()
            .child(body),
    )
}

/// Reopens a closed card, so closing it never loses the update.
pub fn pill(desk: &Desk, ink: &Ink) -> Option<El> {
    let updater = &desk.updater;
    let due = updater.action().is_some() || !matches!(updater.work, Work::Idle | Work::Checking);
    (due && !updater.card_open()).then(|| {
        el().key("update-pill")
            .cursor(rune::Cursor::Pointer)
            .on_press(|d: &mut Desk| d.updater.reopen())
            .child(text("Update ready").font(CAPTION).color(ink.accent))
    })
}

pub fn settings(desk: &Desk, ink: &Ink) -> El {
    let updater = &desk.updater;
    let mut buttons = row().gap(8.0);
    if let Some(action) = updater.action() {
        buttons = buttons.child(act_button(action, "update-act-settings", ink));
    }
    if !updater.busy() {
        buttons = buttons.child(quiet_button(
            "Check now",
            "update-check",
            |d| d.updater.check(),
            ink,
        ));
    }
    let installed = updater
        .cli
        .as_ref()
        .map_or(String::new(), |cli| format!(" · phux {}", cli.current));
    let mut section = col()
        .w_full()
        .gap(8.0)
        .child(section("Updates", ink))
        .child(
            col()
                .w_full()
                .gap(1.0)
                .child(text(headline(updater)).font(LABEL).color(ink.text))
                .child(
                    text(format!("Phux Rune {VERSION}{installed}"))
                        .font(CAPTION)
                        .color(ink.dim),
                ),
        )
        .child(lines(updater, ink));
    if let Work::Downloading(percent) = updater.work {
        section = section.child(progress(percent, 524.0, ink));
    }
    if let Err(reason) = updater.install {
        return section.child(text(reason).font(CAPTION).color(ink.dim));
    }
    section = section.child(buttons).child(toggle(
        "set-update-auto",
        "Check automatically",
        "At launch and every six hours. Nothing installs without a click.",
        updater.automatic,
        Desk::toggle_update_automatic,
        ink,
    ));
    if updater.can_manage_phux() {
        section = section.child(toggle(
            "set-update-phux",
            "Update phux with Phux Rune",
            "Installs the CLI and hands the live server off; sessions keep running.",
            updater.manage_phux,
            Desk::toggle_update_phux,
            ink,
        ));
    }
    section
}
