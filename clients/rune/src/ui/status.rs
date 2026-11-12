//! The window's words about itself: the status line, the bar an action
//! waits in, and what stands in for a terminal that isn't there.

use phux_client_runtime::control::Status;
use rune::{Color, El, Family, TextStyle, col, el, row, text};

use super::{
    CAPTION, Ink, LABEL, LABEL_STRONG, TITLEBAR_H, button, icon_press, icons, keep_from_pane,
    pressable, tabs, tip,
};
use crate::desk::link::{Banner, Standing};
use crate::desk::{Desk, Notice, NoticeKind, Pending};
use crate::keys::Shortcut;
use crate::launch::INSTALL;

mod phone;

/// The terminal's size in the title bar: quiet figures that keep their
/// columns as the window is dragged.
const GEOMETRY: TextStyle = TextStyle::new(10.5, 500.0)
    .family(Family::SystemMono)
    .features("tnum");

/// The least height of something a thumb presses, in points.
const TOUCH: f32 = 44.0;

/// A button as this window is used: on a phone, tall enough to touch.
fn touchable(desk: &Desk, button: El) -> El {
    if desk.phone.is_some() {
        button.min_h(TOUCH)
    } else {
        button
    }
}

/// A notice's words, wrapping; with none it draws nothing.
pub fn notice_line(notice: Option<&Notice>, p: &Ink) -> El {
    let (line, kind) = notice.map_or((String::new(), NoticeKind::Error), |notice| {
        (notice.text.clone(), notice.kind)
    });
    let color = match kind {
        NoticeKind::Info | NoticeKind::Tip => p.text,
        NoticeKind::Error => p.danger,
    };
    text(line)
        // A screen reader reads a notice when it comes: an error at once.
        .role(match kind {
            NoticeKind::Info | NoticeKind::Tip => "status",
            NoticeKind::Error => "alert",
        })
        .font(CAPTION)
        .color(color)
        .wrap()
        .grow(1.0)
        .min_w(0.0)
}

/// What a held action would do, and the word for doing it. A keyboard is
/// told its key; `touch` has none to name.
fn question(pending: &Pending, touch: bool) -> (String, &'static str) {
    let keyed = |bare: &'static str, keyed: &'static str| if touch { bare } else { keyed };
    match pending {
        Pending::Mirror(source, target) => (
            format!(
                "Start {} mirror of {target:?} in a new producer terminal? {} Read-only; no browser/app input or process migration.",
                source.verb(),
                source.prerequisites()
            ),
            "Start",
        ),
        Pending::Paste(_, held) => {
            let lines = held.lines().count().max(1);
            let plural = if lines == 1 { "" } else { "s" };
            (
                format!(
                    "This paste has {lines} line{plural} and would run as typed commands. Paste anyway?"
                ),
                keyed("Paste", "Paste (Return)"),
            )
        }
        Pending::Close(terminal) => (
            format!("Close {terminal}? Anything running in it ends."),
            keyed("Close", "Close (Return)"),
        ),
        Pending::CloseTab(terminals) if terminals.len() == 1 => (
            "Close this tab? Anything running in it ends.".to_owned(),
            keyed("Close", "Close (Return)"),
        ),
        Pending::CloseTab(terminals) => (
            format!(
                "Close this tab and its {} terminals? Anything running in them ends.",
                terminals.len()
            ),
            keyed("Close", "Close (Return)"),
        ),
        Pending::ClipboardRead(terminal, _) => (
            format!(
                "{terminal} wants to read your clipboard.{}",
                keyed("", " A allows it every time.")
            ),
            keyed("Allow once", "Allow once (Return)"),
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            keyed("End session", "End session (Return)"),
        ),
    }
}

/// The action waiting on Return or Escape, floating over the terminals so
/// asking never resizes them.
pub fn confirm_bar(desk: &Desk, p: &Ink) -> Option<El> {
    // A popup covers this place and asks in its own box.
    if desk.popup.command().is_some() {
        return None;
    }
    question_bar(desk, p)
}

/// [`confirm_bar`], wherever its parent puts it.
pub(super) fn question_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let touch = desk.phone.is_some();
    let (message, verb) = question(desk.pending.as_ref()?, touch);
    let cancel = if touch { "Cancel" } else { "Cancel (Esc)" };
    let press = |button: El| touchable(desk, button.grow(0.0).px(10.0));
    Some(
        row()
            .key("confirm-bar")
            .on_drag(keep_from_pane)
            .pin(Some(0.0), Some(0.0), Some(0.0), None)
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .radius(super::CARD_RADIUS)
            .continuous_corners()
            .backdrop_blur(super::FROST)
            .fill(p.raised.alpha(0.82))
            .border(1.0, p.edge)
            .shadow(6.0, 18.0, Color::BLACK.alpha(0.35))
            .transition(rune::Transition::opacity())
            .child(
                text(message)
                    .font(LABEL)
                    .color(p.text)
                    .wrap()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(press(button(
                cancel,
                "confirm-cancel",
                p,
                Desk::cancel_pending,
            )))
            .child(press(
                button(verb, "confirm-ok", p, Desk::confirm_pending)
                    .on_press_tx(Desk::confirm_pending_tx),
            )),
    )
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

/// The install line, to copy.
fn install_line(p: &Ink) -> El {
    row()
        .gap(8.0)
        .items_center()
        .child(
            text(INSTALL)
                .font(CAPTION.family(Family::SystemMono))
                .color(p.text),
        )
        .child(
            pressable("placeholder-copy", p)
                .px(10.0)
                .py(6.0)
                .radius(6.0)
                .border(1.0, p.separator)
                .on_press_tx(|_: &mut Desk, tx| tx.copy(INSTALL.to_owned()))
                .child(text("Copy").font(CAPTION).color(p.text)),
        )
}

fn action(label: &'static str, key: &'static str, p: &Ink, press: fn(&mut Desk)) -> El {
    button(label, key, p, press).grow(0.0).px(12.0)
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
fn remedy(standing: &Standing, updates: bool, p: &Ink) -> Option<El> {
    Some(match standing {
        Standing::Starting | Standing::Installing | Standing::Connecting(_) | Standing::Opening => {
            return None;
        }
        Standing::NoCli(None) => {
            action("Install phux", "placeholder-install", p, Desk::install_phux)
        }
        Standing::NoCli(Some(_)) => {
            action("Try again", "placeholder-install", p, Desk::install_phux)
        }
        Standing::NoSessions => action("New session", "placeholder-session", p, Desk::new_session),
        Standing::NoTerminals => action(
            "New terminal",
            "placeholder-terminal",
            p,
            Desk::new_terminal,
        ),
        Standing::Ended(_) => action("Restore", "placeholder-restore", p, Desk::restore_session),
        Standing::Incompatible(_) if updates => {
            action("Check for updates", "placeholder-update", p, |d| {
                d.updater.check_now();
            })
        }
        Standing::Incompatible(_) | Standing::Unreachable(_) | Standing::Closed(_) => retry(p),
    })
}

fn retry(p: &Ink) -> El {
    action("Retry", "placeholder-retry", p, Desk::reconnect)
}

pub fn placeholder(desk: &Desk, p: &Ink) -> El {
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
    let color = if failed { p.danger } else { p.dim };
    let actions = repair
        .then(|| phone::pair_again("placeholder-pair", p))
        .into_iter()
        .chain(remedy(&standing, updates, p))
        // Words under the phone's dial mean it has gone on long enough to
        // act on.
        .chain(phone::slow_dial(desk, &standing).then(|| retry(p)))
        // A server updated from elsewhere never says so.
        .chain(updates.then(|| retry(p)))
        // Pairing again already goes where the phone's computers are.
        .chain((!repair).then(|| {
            action(
                elsewhere,
                "placeholder-machines",
                p,
                Desk::toggle_machines,
            )
        }));
    let buttons = row()
        .w_full()
        .center()
        .gap(8.0)
        .pt(6.0)
        .children(actions.map(|button| touchable(desk, button)));
    col()
        .gap(6.0)
        .items_center()
        .max_w(520.0)
        .px(if desk.phone.is_some() { 16.0 } else { 0.0 })
        .child(text(title).font(TextStyle::new(15.0, 600.0)).color(color))
        .child(text(detail).font(LABEL).color(p.dim).wrap())
        .children(matches!(standing, Standing::NoCli(_)).then(|| install_line(p)))
        .child(buttons)
}

/// Over panes that have stopped updating: what happened, and a way to act.
/// It floats, so it never resizes a terminal.
pub fn banner(desk: &Desk, p: &Ink) -> Option<El> {
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
        .chain(repair.then(|| phone::pair_again("banner-pair", p)))
        .chain([action("Retry", "banner-retry", p, Desk::reconnect)]);
    Some(
        col()
            .absolute(0.0, phone::banner_top(desk))
            .w_full()
            .px(12.0)
            .items_center()
            .transition(rune::Transition::opacity())
            .child(
                row()
                    .key("reconnect-banner")
                    .on_drag(keep_from_pane)
                    .w_full()
                    .max_w(if update { 560.0 } else { 420.0 })
                    .px(14.0)
                    .py(8.0)
                    .gap(12.0)
                    .radius(9.0)
                    .items_center()
                    .fill(p.raised)
                    .border(1.0, p.separator)
                    .child(crate::host_color::dot(machine, 8.0, p.raised))
                    .child(
                        col()
                            .gap(2.0)
                            .grow(1.0)
                            .min_w(0.0)
                            .child(text(lost).font(LABEL_STRONG).color(p.text).truncate())
                            .child(text(why).font(CAPTION).color(p.dim).wrap()),
                    )
                    .children(actions.map(|button| touchable(desk, button))),
            ),
    )
}

/// How the link is, when it isn't simply up: a healthy window says nothing.
fn link_trouble(desk: &Desk, p: &Ink) -> Option<(&'static str, Color)> {
    match desk.status {
        _ if desk.client.is_none() && desk.link_word() != "connecting" => {
            Some(("not connected", p.dim))
        }
        Status::Attached => None,
        Status::Idle | Status::Connecting => Some(("connecting", p.dim)),
        Status::Negotiated => Some(("negotiated", p.dim)),
        Status::Closed => Some(("closed", p.dim)),
        Status::Failed => Some(("failed", p.danger)),
    }
}

/// What is on screen, unless tabs say that already.
fn place(desk: &Desk, p: &Ink) -> El {
    let session = desk.selected_session_name().unwrap_or_default();
    let title = desk.window_title().to_owned();
    // The window title ends in its session; here that sits in front.
    let title = title
        .strip_suffix(&format!(" — {session}"))
        .map_or(title.clone(), str::to_owned);
    // The terminal's title grows from nothing into what is left, so a narrow
    // window cuts it before it cuts the session's name.
    // With no terminal on screen the window title is the app's own name.
    let terminal = (desk.shown.is_some() && title != session && !title.is_empty()).then(|| {
        el().w(0.0)
            .grow(1.0)
            .min_w(0.0)
            .child(text(title).font(LABEL).color(p.dim).truncate())
    });
    row()
        .gap(7.0)
        .min_w(0.0)
        .shrink()
        .grow(8.0)
        .items_center()
        .child(text(session).font(LABEL_STRONG).color(p.text).truncate())
        .children(terminal)
}

/// The band over the terminal: draggable, under the traffic lights when the
/// sidebar is folded, and holding what a status line would.
pub fn title_bar(desk: &Desk, p: &Ink, lights: f32) -> El {
    let geometry = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view?))
        .map(|frame| format!("{}×{}", frame.cols, frame.rows))
        .unwrap_or_default();
    let folded = !desk.sidebar_shown();
    let sidebar = icon_press(
        &icons::SIDEBAR,
        "Sidebar",
        "sidebar-toggle",
        p,
        Desk::toggle_sidebar,
    )
    .expanded(desk.sidebar_shown());
    let settings = icon_press(
        &icons::SETTINGS,
        "Settings",
        "settings-button",
        p,
        Desk::toggle_settings,
    );
    // Where a ⌘-click goes, as a browser shows it: in the room the tabs
    // leave, so pointing at a link never squeezes them.
    let link = desk.pointer.hover.as_ref().map(|(_, link)| {
        text(link.url.clone())
            .font(CAPTION)
            .color(p.accent)
            .truncate()
    });
    // Everything between the sidebar's button and the window's own words:
    // its width is the room the tabs are cut from.
    let lane = row()
        .key("titlebar-lane")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .gap(8.0)
        .items_center()
        .on_layout(|d: &mut Desk, rect| d.tabs.room = rect[2])
        .children(tabs::strip(desk, p).or_else(|| Some(place(desk, p))))
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .justify_end()
                .items_center()
                .children(link),
        );
    row()
        .key("titlebar")
        .w_full()
        .h(TITLEBAR_H)
        .pl(if folded { lights + 8.0 } else { 2.0 })
        .pr(10.0)
        .gap(8.0)
        .items_center()
        .window_drag()
        .child(tip(sidebar, "Sidebar", Shortcut::ToggleSidebar))
        .child(lane)
        .children(link_trouble(desk, p).map(|(word, color)| text(word).font(CAPTION).color(color)))
        .children(super::updates::pill(desk, p))
        .child(
            text(geometry)
                .font(GEOMETRY)
                .color(p.dim.alpha(0.8))
                .a11y_hidden(),
        )
        .child(
            row()
                .gap(2.0)
                .items_center()
                .child(super::news::bell(desk, p))
                .child(tip(settings, "Settings", Shortcut::Settings)),
        )
}

#[cfg(test)]
mod tests;
