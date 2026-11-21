//! The window's words about itself: the status line, the bar an action
//! waits in, and what stands in for a terminal that isn't there.

use crate::ui::tokens::Voice;
use crate::ui::tokens::layout::STATUS;
use phux_design::State;
use rune::{El, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Progress};

use super::Tokens;

use super::tokens::{MONO, Tier};
use super::{Paint, badge, button, icon_press, icons, keep_from_pane, tabs, tip};
use crate::desk::link::{Banner, Standing};
use crate::desk::{Desk, Pending};
use crate::keys::Shortcut;
use crate::launch::INSTALL;

mod confirm;
mod phone;
pub(in crate::ui) use phone::empty as phone_empty;
mod titlebar;
pub use confirm::confirm_bar;
#[cfg(test)]
use confirm::question;
pub(in crate::ui) use confirm::question_bar;
pub use titlebar::{hover_toolbar, tab_row, top};
#[cfg(test)]
mod chrome_gallery;

/// A button as this window is used: on a phone, big enough to touch.
fn touchable(desk: &Desk, button: El) -> El {
    if desk.phone.is_some() {
        let min = phux_design::tokens::SPACE.touch_min;
        button.min_h(min).min_w(min)
    } else {
        button
    }
}

/// The refusal says the server is the one behind.
fn server_behind(words: &str) -> bool {
    words.contains("update the phux server")
}

/// Whether this computer's updates can fix `standing`. The app can always
/// catch up, but a server behind on another machine updates there.
fn updates_can_fix(standing: &Standing, remote: bool) -> bool {
    match standing {
        Standing::Incompatible(words) => !(remote && server_behind(words)),
        _ => false,
    }
}
/// What stands in for the terminal: the words, then what can be done.
fn words(standing: &Standing, machine: &str, remote: bool) -> (&'static str, String) {
    // Every use below is mid-sentence, where the local label reads lowercase.
    let machine = if machine == crate::machines::LOCAL {
        if cfg!(target_os = "macos") {
            "this Mac"
        } else {
            "this computer"
        }
    } else {
        machine
    };
    match standing {
        Standing::Starting => ("Starting phux…", String::new()),
        Standing::Installing => (
            "Installing phux…",
            "Downloading and checking the latest release. This takes a minute.".to_owned(),
        ),
        Standing::NoCli(None) => (
            "phux isn't installed",
            "This app runs on the phux command-line tool. Install it here, or paste the line \
             below into Terminal; either way this window connects on its own."
                .to_owned(),
        ),
        Standing::NoCli(Some(why)) => (
            "phux didn't install",
            format!("The installer said: {why}. Check your connection and try again."),
        ),
        // The server says which side is behind.
        Standing::Incompatible(words) if server_behind(words) && remote => (
            "Update the phux server",
            format!(
                "This app is newer than the server on {machine}, and updating it happens there: \
                 run `phux update` on {machine}, then retry. {words}"
            ),
        ),
        Standing::Incompatible(words) if server_behind(words) => (
            "Update the phux server",
            format!("This app is newer than the server on {machine}. {words}"),
        ),
        Standing::Incompatible(words) => (
            "Update Phux Rune",
            format!(
                "The server on {machine} was upgraded past this app, so they can't talk. Your \
                 sessions are safe. {words}"
            ),
        ),
        Standing::Unreachable(why) => ("Can't reach phux", why.clone()),
        Standing::Connecting(why) => ("Connecting…", why.clone()),
        Standing::Closed(why) => ("Disconnected", why.clone()),
        Standing::NoSessions => (
            "No sessions",
            format!("Nothing is running on {machine} yet."),
        ),
        Standing::NoTerminals => (
            "No terminals",
            "This session is empty; start a terminal in it.".to_owned(),
        ),
        Standing::Ended(1) => (
            "The server restarted",
            "It ended this session's terminal. Restore starts it again.".to_owned(),
        ),
        Standing::Ended(n) => (
            "The server restarted",
            format!("It ended this session's {n} terminals. Restore starts them again."),
        ),
        Standing::Opening => ("Opening terminal…", String::new()),
    }
}

/// An indeterminate bar for a wait with no known end (kit `Progress`): a
/// segment sweeping a track on the render clock. Reduced motion holds it.
fn progress(label: &'static str, t: &Tokens) -> El {
    Progress::new(None)
        .build(t.controls(false))
        .key("placeholder-progress")
        .label(label)
        .w(STATUS.progress_w)
}

/// The install line, to copy: a command, so in the mono face.
fn install_line(t: &Tokens) -> El {
    row()
        .gap(t.space.s2)
        .items_center()
        .child(
            text(INSTALL)
                .font(t.ty.caption.family(MONO))
                .color(t.roles.text.text.paint()),
        )
        .child(
            Button::new("Copy")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .on_press_tx(|_: &mut Desk, tx| tx.copy(INSTALL.to_owned()))
                .build(&t.kit)
                .key("placeholder-copy"),
        )
}

/// One of the window's own actions: the shared outlined button, sized to
/// its words, a Tab stop.
fn action(label: &'static str, key: &'static str, p: &Tokens, press: fn(&mut Desk)) -> El {
    button(label, key, p, press).grow(0.0).label(label)
}

/// Something the window offers to do about where it stands, before it is
/// drawn: a desktop's outlined button, or a phone's full-width one.
#[derive(Clone, Copy)]
struct Act {
    label: &'static str,
    key: &'static str,
    press: fn(&mut Desk),
    /// The remedy itself, not a way elsewhere: a phone leads with it.
    primary: bool,
}

impl Act {
    const fn new(label: &'static str, key: &'static str, press: fn(&mut Desk)) -> Self {
        Self {
            label,
            key,
            press,
            primary: true,
        }
    }

    const fn secondary(self) -> Self {
        Self {
            primary: false,
            ..self
        }
    }

    fn button(self, p: &Tokens) -> El {
        action(self.label, self.key, p, self.press)
    }
}

/// The title and detail for `standing`, in this window's voice: the phone
/// app has its own for what a desktop would answer with a command.
fn said(desk: &Desk, standing: &Standing) -> (String, String) {
    let machine = desk.machines.label();
    let on_phone = desk
        .native
        .as_ref()
        .and_then(|_| phone::words(standing, machine, desk.unpaired()));
    on_phone.unwrap_or_else(|| {
        let (title, detail) = words(standing, machine, desk.machines.current.is_some());
        (title.to_owned(), detail)
    })
}

/// The one thing to do about `standing`, when there is one. `updates` is
/// whether this computer's can fix it.
fn remedy(standing: &Standing, updates: bool) -> Option<Act> {
    Some(match standing {
        Standing::Starting | Standing::Installing | Standing::Connecting(_) | Standing::Opening => {
            return None;
        }
        Standing::NoCli(None) => {
            Act::new("Install phux", "placeholder-install", Desk::install_phux)
        }
        Standing::NoCli(Some(_)) => {
            Act::new("Try again", "placeholder-install", Desk::install_phux)
        }
        Standing::NoSessions => Act::new("New session", "placeholder-session", Desk::new_session),
        Standing::NoTerminals => {
            Act::new("New terminal", "placeholder-terminal", Desk::new_terminal)
        }
        Standing::Ended(_) => Act::new("Restore", "placeholder-restore", Desk::restore_session),
        Standing::Incompatible(_) if updates => {
            Act::new("Check for updates", "placeholder-update", |d| {
                d.updater.check_now();
            })
        }
        Standing::Incompatible(_) | Standing::Unreachable(_) | Standing::Closed(_) => retry(),
    })
}

const fn retry() -> Act {
    Act::new("Retry", "placeholder-retry", Desk::reconnect)
}

/// A placeholder's title: a failure leads with its mark and reads in the
/// failed tone (colour, word and mark together); anything else is
/// quiet.
fn title_line(title: String, failed: bool, t: &Tokens) -> El {
    let font = t.ty.lead.semibold();
    if !failed {
        return text(title)
            .font(font)
            .color(t.roles.text.text.paint())
            .align(rune::Align::Center)
            .wrap()
            .role("heading");
    }
    row()
        .gap(t.space.icon_gap)
        .items_center()
        .role("heading")
        .label(title.clone())
        .child(badge::mark(t, State::Failed, t.icon.size.md))
        .child(
            text(title)
                .font(font)
                .color(t.roles.status.failed.paint())
                .a11y_hidden(),
        )
}

/// What stands in for the terminal, and what can be done about it.
pub fn placeholder(desk: &Desk, p: &Tokens) -> El {
    let t = p;
    let standing = desk.standing();
    let native = desk.native.is_some();
    let (title, detail) = said(desk, &standing);
    // The phone app has no updater, and its other computers are in its host.
    let updates = !native && updates_can_fix(&standing, desk.machines.current.is_some());
    let repair = phone::repairs(desk, &standing);
    let elsewhere = if native {
        "Change computer…"
    } else {
        "Other machines…"
    };
    let failed = matches!(
        standing,
        Standing::Unreachable(_) | Standing::Incompatible(_) | Standing::NoCli(Some(_))
    );
    let acts: Vec<Act> = repair
        .then(|| phone::pair_again("placeholder-pair"))
        .into_iter()
        .chain(remedy(&standing, updates))
        // Words under the phone's dial mean it has gone on long enough to
        // act on.
        .chain(phone::slow_dial(desk, &standing).then(retry))
        // A server updated from elsewhere never says so.
        .chain(updates.then(retry))
        // Pairing again already goes where the phone's computers are.
        .chain((!repair).then(|| {
            Act::new(elsewhere, "placeholder-machines", Desk::toggle_machines).secondary()
        }))
        .collect();
    let extra = match standing {
        Standing::Installing => Some(progress("Installing phux", t)),
        Standing::Starting => Some(progress("Starting phux", t)),
        Standing::NoCli(_) => Some(install_line(t)),
        _ => None,
    };
    if desk.phone.is_some() {
        let said = phone::Said {
            standing: &standing,
            title,
            detail,
            failed,
        };
        return phone::stage(&said, &acts, extra, t);
    }
    let s = &t.space;
    let buttons = row()
        .w_full()
        .center()
        .gap(s.s2)
        .pt(s.icon_gap)
        .children(acts.into_iter().map(|act| act.button(p)));
    // Inset from the pane's edge whatever its width, heading and body both
    // centred on one measure.
    col()
        .gap(s.icon_gap)
        .items_center()
        .w_full()
        .max_w(STATUS.placeholder_w)
        .px(s.sheet_pad)
        .child(title_line(title, failed, t))
        .child(
            text(detail)
                .font(t.ty.label)
                .color(t.roles.text.dim.paint())
                .align(rune::Align::Center)
                .wrap(),
        )
        .children(extra)
        .child(buttons)
}

/// A phone's pane whose first frame has not arrived: the stage's calm
/// opening state, never drawn over terminal text.
pub fn opening(p: &Tokens) -> El {
    let said = phone::Said {
        standing: &Standing::Opening,
        title: "Opening terminal…".to_owned(),
        detail: String::new(),
        failed: false,
    };
    phone::stage(&said, &[], None, p)
}

/// Over panes that have stopped updating: what happened, and a way to act.
/// It floats, so it never resizes a terminal. On the raised tier, opaque.
pub fn banner(desk: &Desk, p: &Tokens) -> Option<El> {
    let t = p;
    let s = &t.space;
    let banner = desk.banner()?;
    let machine = desk.machines.label();
    let remote = desk.machines.current.is_some();
    let (lost, why) = match &banner {
        // Short, so a narrow window still shows the state.
        Banner::Reconnecting(why) => (format!("Reconnecting to {machine}…"), why.clone()),
        Banner::Stuck(standing) => said(desk, standing),
    };
    // The phone app has no updater to check with.
    let update = desk.native.is_none()
        && matches!(&banner, Banner::Stuck(standing) if updates_can_fix(standing, remote));
    let repair = matches!(&banner, Banner::Stuck(standing) if phone::repairs(desk, standing));
    let actions = update
        .then(|| {
            action("Check for updates", "banner-update", p, |d| {
                d.updater.check_now();
            })
        })
        .into_iter()
        .chain(repair.then(|| phone::pair_again("banner-pair").button(p)))
        .chain([action("Retry", "banner-retry", p, Desk::reconnect)]);
    let raised = t.roles.layer.raised.paint();
    let surface = row()
        .key("reconnect-banner")
        .on_drag(keep_from_pane)
        .w_full()
        .max_w(if update {
            STATUS.update_banner_w
        } else {
            STATUS.banner_w
        })
        .px(s.card_inset)
        .py(s.s2)
        .gap(s.s3)
        .radius(t.radius.md)
        .continuous_corners()
        .items_center()
        .fill(raised)
        .border(s.hairline, t.roles.border.base.paint())
        .child(crate::host_color::dot(machine, s.s2, raised))
        .child(
            col()
                .gap(s.s0_5)
                .grow(1.0)
                .min_w(0.0)
                .child(
                    text(lost)
                        .font(t.ty.label.semibold())
                        .color(t.roles.text.text.paint())
                        .truncate(),
                )
                .child(
                    text(why)
                        .font(t.ty.caption)
                        .color(t.roles.text.dim.paint())
                        .wrap(),
                ),
        )
        .children(actions.map(|button| touchable(desk, button)));
    Some(
        col()
            .absolute(0.0, phone::banner_top(desk))
            .w_full()
            .px(s.s3)
            .items_center()
            .transition(rune::Transition::opacity())
            .child(t.elevate(surface, Tier::Raised)),
    )
}

#[cfg(test)]
mod tests;
