//! Updates on screen: a corner card that never takes the keyboard by
//! itself, the status-line pill that reopens it, and the section in
//! settings.

mod phux;

use super::tokens::layout::UPDATES as GEOMETRY;
use crate::ui::tokens::Voice;
use rune::{El, TextStyle, Transition, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Progress};

use super::settings::{Toggle, grown, kit_button, primary, secondary, section, toggle};
use super::tokens::{Paint, Tier, Tokens};
use crate::desk::Desk;
use crate::desk::updates::{Action, Updater, Work};
use crate::updates::VERSION;

/// The update's one primary action, a kit `Submit` button in the Tab
/// order.
fn act_button(action: Action, key: &'static str, t: &Tokens) -> El {
    kit_button(key, action.label(), ButtonVariant::Submit, t)
        .on_press_tx(|d: &mut Desk, tx| d.update_act(tx))
}

/// Real transfer progress only; verifying has no percentage to show.
fn progress(percent: u8, t: &Tokens) -> El {
    Progress::new(Some(f32::from(percent) / 100.0))
        .build(t.controls(false))
        .key("update-progress")
        .label("Download progress")
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

fn lines(desk: &Desk, t: &Tokens, style: TextStyle) -> El {
    let remote = desk.machines.current.as_deref();
    let mut list = col().w_full().gap(t.space.s0_5);
    for line in detail(&desk.updater, remote) {
        list = list.child(
            text(line)
                .font(style)
                .color(t.roles.text.dim.paint())
                .wrap(),
        );
    }
    list
}

pub fn card(desk: &Desk, p: &Tokens, top: f32) -> Option<El> {
    let t = p;
    let s = &t.space;
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
        .w(GEOMETRY.card_w)
        .padding(s.card_inset)
        .gap(s.s2)
        .radius(t.radius.md)
        .fill(t.roles.layer.raised.paint())
        .border(s.hairline, t.roles.border.base.paint())
        .transition(Transition::opacity())
        .child(
            row()
                .w_full()
                .gap(s.s2)
                .items_start()
                .child(
                    text(headline(updater))
                        .font(t.ty.label.semibold())
                        .color(t.roles.text.text.paint())
                        .wrap()
                        .grow(1.0),
                )
                .child(super::icon_press(
                    &super::icons::CLOSE,
                    "Dismiss",
                    "update-dismiss",
                    t,
                    |d: &mut Desk| d.updater.dismiss(),
                )),
        )
        .child(lines(desk, t, t.ty.caption))
        .children(percent.map(|percent| progress(percent, t).key("update-card-progress")))
        .children(
            updater
                .action()
                .map(|action| row().child(act_button(action, "update-act", t))),
        );
    Some(
        row()
            .key("update-corner")
            .absolute(0.0, top + s.s2)
            .w_full()
            .pr(s.card_inset)
            .justify_end()
            .child(t.elevate(body, Tier::Raised)),
    )
}

/// Reopens a closed card, so closing it never loses the update.
pub fn pill(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    let updater = &desk.updater;
    let due = updater.action().is_some() || !matches!(updater.work, Work::Idle | Work::Checking);
    (due && !updater.card_open()).then(|| {
        Button::new("Update ready")
            .variant(ButtonVariant::Submit)
            .control_size(ControlSize::Small)
            .on_press(|d: &mut Desk| d.updater.reopen())
            .build(&t.kit)
            .key("update-pill")
            .focusable(true)
            .min_h(grown(t.ty.caption, t.space.icon_gap))
            .radius(t.radius.full)
    })
}

pub fn settings(desk: &Desk, t: &Tokens) -> El {
    let s = &t.space;
    let dim = t.roles.text.dim.paint();
    if desk.native.is_some() {
        return col().w_full().gap(s.s2)
            .child(section("Updates", t))
            .child(text(format!("Phux Rune {VERSION}")).font(primary(desk, t)).color(t.roles.text.text.paint()))
            .child(text("Native iOS builds update through their distribution channel. This app never installs a desktop binary or starts a local server.")
                .font(secondary(desk, t)).color(dim).wrap());
    }
    let updater = &desk.updater;
    let mut buttons = row().w_full().gap(s.s2).flex_wrap();
    if let Some(action) = updater.action() {
        let mut button = act_button(action, "update-act-settings", t).min_h(s.touch_min);
        if desk.phone.is_some() {
            button = button.w_full().min_w(0.0);
        }
        buttons = buttons.child(button);
    }
    if !updater.busy() {
        buttons = buttons.child(
            kit_button("update-check", "Check now", ButtonVariant::Outline, t)
                .min_h(s.touch_min)
                .on_press(|d: &mut Desk| d.updater.check_now()),
        );
    }
    let installed = updater
        .cli
        .as_ref()
        .map_or(String::new(), |cli| format!(" · phux {}", cli.current));
    let mut section = col()
        .w_full()
        .gap(s.s2)
        .child(section("Updates", t).font(super::settings::heading_type(desk, t)))
        .child(
            col()
                .w_full()
                .gap(s.s0_5)
                .child(
                    text(headline(updater))
                        .font(primary(desk, t).medium())
                        .color(t.roles.text.text.paint())
                        .wrap(),
                )
                .child(
                    text(format!("Phux Rune {VERSION}{installed}"))
                        .font(secondary(desk, t))
                        .color(dim)
                        .wrap(),
                ),
        )
        .child(lines(desk, t, secondary(desk, t)));
    if let Work::Downloading(percent) = updater.work {
        section = section.child(progress(percent, t));
    }
    if let Err(reason) = updater.install {
        return section.child(text(reason).font(secondary(desk, t)).color(dim).wrap());
    }
    let automatic = Toggle {
        key: "set-update-auto",
        title: "Check automatically",
        detail: "At launch and every six hours. Nothing installs without a click.",
        on: updater.automatic,
        press: Desk::toggle_update_automatic,
        enabled: true,
    };
    section = section
        .child(buttons)
        // The section's card supplies the row gutter, on a phone and a
        // desk alike: the toggle lines up with the words above it.
        .child(toggle(desk, automatic, t).px(0.0));
    section.child(phux::settings(desk, t))
}

/// Failures and compatibility warnings stay in view even when search hides
/// the About & Updates category. Informational update details stay there.
/// The invalid treatment: a `status.failed` edge, the words in
/// `text`.
pub(super) fn warnings(desk: &Desk, t: &Tokens) -> Option<El> {
    let s = &t.space;
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
    let failed = t.status.tone(phux_design::Tone::Failed);
    Some(
        col()
            .key("settings-update-warning")
            .w_full()
            .padding(s.s4)
            .gap(s.s1)
            .radius(t.radius.md)
            .fill(t.roles.layer.raised.paint())
            .border(s.hairline, failed.ink)
            .role("alert")
            .label(messages.join(" "))
            .children(messages.into_iter().map(|message| {
                text(message)
                    .font(secondary(desk, t))
                    .color(t.roles.text.text.paint())
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
        let desk = Desk::with_preferences(
            &Resolved::offline("not launched"),
            None,
            rune::prefs::Prefs::memory(),
        );
        assert!(detail(&desk.updater, Some("mini")).is_empty());
    }

    /// Every pressable is focusable, the card's action included;
    /// opening the card never moves focus into it.
    #[test]
    fn main_window_update_card_joins_tab_order_without_taking_focus() {
        let mut desk = Desk::with_preferences(
            &Resolved::offline("update card focus"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.updater.install = Ok("/not-an-installed-app".into());
        desk.updater.staged = Some("999.0.0".into());
        let mut rt = crate::test_support::runtime(
            desk,
            crate::ui::view,
            (1100.0, 720.0),
            rune::Env::new(false, 1.0),
        );
        rt.frame(0.0);
        let action = crate::ui::element(&rt, "update-act");
        assert!(action.focusable, "the card's action is a Tab stop");
        assert!(!action.focused, "the card never takes the keyboard");
        assert!(crate::ui::element(&rt, "update-dismiss").focusable);
    }
}
