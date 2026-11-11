//! Updates on screen: a corner card that never takes the keyboard, the
//! status-line pill that reopens it, and the section in settings.

mod phux;

use rune::{Color, El, TextStyle, Transition, col, el, row, text};

use super::settings::{primary, secondary, section, toggle};
use super::{CAPTION, Ink, LABEL, LABEL_STRONG, pressable};
use crate::desk::Desk;
use crate::desk::updates::{Action, Updater, Work};
use crate::updates::VERSION;

const CARD_W: f32 = 320.0;

fn act_button(action: Action, key: &'static str, ink: &Ink, style: TextStyle) -> El {
    el().key(key)
        .label(action.label())
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .fill(ink.accent)
        .cursor(rune::Cursor::Pointer)
        .on_press_tx(|d: &mut Desk, tx| d.update_act(tx))
        .child(text(action.label()).font(style).color(ink.on_accent).wrap())
}

fn quiet_button(
    label: &'static str,
    key: &'static str,
    press: fn(&mut Desk),
    ink: &Ink,
    style: TextStyle,
) -> El {
    pressable(key, ink)
        .focusable(true)
        .label(label)
        .min_h(44.0)
        .px(12.0)
        .py(6.0)
        .radius(6.0)
        .border(1.0, ink.separator)
        .on_press(press)
        .child(text(label).font(style).color(ink.text))
}

/// Real transfer progress only; verifying has no percentage to show.
fn progress(percent: u8, ink: &Ink) -> El {
    el().key("update-progress")
        .w_full()
        .h(3.0)
        .radius(1.5)
        .fill(ink.separator)
        .child(
            el().w_frac(f32::from(percent) / 100.0)
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
        // Never "up to date" over an error: the lines below say what failed.
        (_, None, _) if updater.message.is_some() => "Couldn't check for updates".to_owned(),
        (_, _, Some(version)) if updater.staged.is_some() => {
            format!("Phux Rune {version} installs on restart")
        }
        (_, _, Some(version)) => format!("Phux Rune {version} is available"),
        _ if updater.release.is_some() => "You're up to date".to_owned(),
        _ => "Not checked yet".to_owned(),
    }
}

/// What the update means for the sessions and the server. The window's own
/// machine may be `remote`, and phux here is always this computer's.
fn detail(updater: &Updater, remote: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(machine) = remote.filter(|_| updater.phux_due()) {
        lines.push(format!(
            "This updates phux on this computer, not on {machine}."
        ));
    }
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

fn lines(desk: &Desk, ink: &Ink, style: TextStyle) -> El {
    let remote = desk.machines.current.as_deref();
    let mut list = col().w_full().gap(2.0);
    for line in detail(&desk.updater, remote) {
        list = list.child(text(line).font(style).color(ink.dim).wrap());
    }
    list
}

pub fn card(desk: &Desk, ink: &Ink, top: f32) -> Option<El> {
    let updater = &desk.updater;
    if !updater.card_open() {
        return None;
    }
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
                        .child(
                            rune::icon(&super::icons::CLOSE, 12.0)
                                .color(ink.dim)
                                .a11y_hidden(),
                        ),
                ),
        )
        .child(lines(desk, ink, CAPTION))
        .children(percent.map(|percent| progress(percent, ink).key("update-card-progress")))
        .children(
            updater
                .action()
                .map(|action| row().child(act_button(action, "update-act", ink, CAPTION))),
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
        super::pressable("update-pill", ink)
            .h(20.0)
            .px(8.0)
            .radius(10.0)
            .center()
            .fill(ink.accent.alpha(0.16))
            .on_press(|d: &mut Desk| d.updater.reopen())
            .child(text("Update ready").font(CAPTION).color(ink.accent))
    })
}

pub fn settings(desk: &Desk, ink: &Ink) -> El {
    if desk.native.is_some() {
        return col().w_full().gap(8.0)
            .child(section("Updates", ink))
            .child(text(format!("Phux Rune {VERSION}")).font(primary(desk, LABEL)).color(ink.text))
            .child(text("Native iOS builds update through their distribution channel. This app never installs a desktop binary or starts a local server.")
                .font(secondary(desk, CAPTION)).color(ink.dim).wrap());
    }
    let updater = &desk.updater;
    // The containing phone card already supplies its row gutter.
    let toggle_inset = if desk.phone.is_some() { 0.0 } else { 16.0 };
    let mut buttons = row().w_full().gap(8.0).flex_wrap();
    if let Some(action) = updater.action() {
        let mut button = act_button(action, "update-act-settings", ink, primary(desk, CAPTION))
            .focusable(true)
            .min_h(44.0);
        if desk.phone.is_some() {
            button = button.w_full().min_w(0.0);
        }
        buttons = buttons.child(button);
    }
    if !updater.busy() {
        buttons = buttons.child(quiet_button(
            "Check now",
            "update-check",
            |d| d.updater.check_now(),
            ink,
            primary(desk, CAPTION),
        ));
    }
    let installed = updater
        .cli
        .as_ref()
        .map_or(String::new(), |cli| format!(" · phux {}", cli.current));
    let mut section = col()
        .w_full()
        .gap(8.0)
        .child(section("Updates", ink).font(secondary(desk, TextStyle::new(12.0, 600.0))))
        .child(
            col()
                .w_full()
                .gap(1.0)
                .child(
                    text(headline(updater))
                        .font(primary(desk, LABEL))
                        .color(ink.text)
                        .wrap(),
                )
                .child(
                    text(format!("Phux Rune {VERSION}{installed}"))
                        .font(secondary(desk, CAPTION))
                        .color(ink.dim)
                        .wrap(),
                ),
        )
        .child(lines(desk, ink, secondary(desk, CAPTION)));
    if let Work::Downloading(percent) = updater.work {
        section = section.child(progress(percent, ink));
    }
    if let Err(reason) = updater.install {
        return section.child(
            text(reason)
                .font(secondary(desk, CAPTION))
                .color(ink.dim)
                .wrap(),
        );
    }
    section = section.child(buttons).child(
        toggle(
            desk,
            "set-update-auto",
            "Check automatically",
            "At launch and every six hours. Nothing installs without a click.",
            updater.automatic,
            Desk::toggle_update_automatic,
            ink,
        )
        .px(toggle_inset),
    );
    section.child(phux::settings(desk, ink, toggle_inset))
}

/// Failures and compatibility warnings stay in view even when search hides
/// the About & Updates category. Informational update details stay there.
pub(super) fn warnings(desk: &Desk, ink: &Ink) -> Option<El> {
    let updater = &desk.updater;
    let mut messages: Vec<_> = updater.message.iter().cloned().collect();
    if let Some((now, incoming)) = updater.protocol_warning() {
        messages.push(format!(
            "Protocol {now} → {incoming}. The server needs updating too."
        ));
    }
    if messages.is_empty() {
        return None;
    }
    Some(
        col()
            .key("settings-update-warning")
            .w_full()
            .padding(16.0)
            .gap(6.0)
            .radius(12.0)
            .fill(ink.raised)
            .border(1.0, ink.danger)
            .role("alert")
            .label(messages.join(" "))
            .children(messages.into_iter().map(|message| {
                text(message)
                    .font(secondary(desk, CAPTION))
                    .color(ink.text)
                    .wrap()
                    .a11y_hidden()
            })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    #[test]
    fn nothing_extra_is_said_with_no_server_update_due() {
        let desk = Desk::new(&Resolved::offline("not launched"));
        assert!(detail(&desk.updater, Some("mini")).is_empty());
    }

    #[test]
    fn main_window_update_card_never_adds_a_tab_stop() {
        let mut desk = Desk::new(&Resolved::offline("update card focus"));
        desk.updater.install = Ok("/not-an-installed-app".into());
        desk.updater.staged = Some("999.0.0".into());
        let mut rt = rune::Runtime::new(
            desk,
            crate::ui::view,
            (1100.0, 720.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        let action = rt
            .elements()
            .into_iter()
            .find(|el| el.key.as_deref() == Some("update-act"))
            .unwrap();
        assert!(
            !action.focusable,
            "only the Settings update action participates in Tab order"
        );
    }
}
