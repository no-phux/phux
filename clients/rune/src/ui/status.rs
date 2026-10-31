//! The window's words about itself: the status line, the bar an action
//! waits in, and what stands in for a terminal that isn't there.

use phux_client_runtime::control::Status;
use rune::{El, Family, TextStyle, col, el, row, text};

use super::{CAPTION, Ink, LABEL, LABEL_STRONG, button, pressable};
use crate::desk::link::Standing;
use crate::desk::{Desk, Notice, NoticeKind, Pending};
use crate::launch::INSTALL;

/// A notice as one line; with none it draws nothing.
pub fn notice_line(notice: Option<&Notice>, p: &Ink) -> El {
    let (line, kind) = notice.map_or((String::new(), NoticeKind::Error), |notice| {
        (notice.text.clone(), notice.kind)
    });
    let color = match kind {
        NoticeKind::Info => p.dim,
        NoticeKind::Error => p.danger,
    };
    text(line).font(CAPTION).color(color).truncate()
}

/// A held paste: what it would do, and the choice.
/// The action waiting on Return or Escape: what it would do, and the choice.
pub fn confirm_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let (message, verb) = match desk.pending.as_ref()? {
        Pending::Paste(_, held) => {
            let lines = held.lines().count().max(1);
            let plural = if lines == 1 { "" } else { "s" };
            (
                format!(
                    "This paste has {lines} line{plural} and would run as typed commands. Paste anyway?"
                ),
                "Paste (Return)",
            )
        }
        Pending::Close(terminal) => (
            format!("Close {terminal}? Anything running in it ends."),
            "Close (Return)",
        ),
        Pending::CloseTab(terminals) => (
            format!(
                "Close this tab's {} terminal(s)? Anything running ends.",
                terminals.len()
            ),
            "Close (Return)",
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            "End session (Return)",
        ),
    };
    Some(
        row()
            .key("confirm-bar")
            .w_full()
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .fill(p.selected)
            .transition(rune::Transition::opacity())
            .child(text(message).font(LABEL).color(p.text).truncate())
            .child(el().grow(1.0))
            .child(
                button("Cancel (Esc)", "confirm-cancel", p, Desk::cancel_pending)
                    .grow(0.0)
                    .px(10.0),
            )
            .child(
                button(verb, "confirm-ok", p, Desk::confirm_pending)
                    .grow(0.0)
                    .px(10.0),
            ),
    )
}

/// What stands in for the terminal: the words, then what can be done.
fn words(standing: &Standing, machine: &str) -> (&'static str, String) {
    match standing {
        Standing::Starting => ("Starting phux…", String::new()),
        Standing::NoCli => (
            "phux isn't installed",
            "This app runs on the phux command-line tool. Install it, and this window connects \
             on its own."
                .to_owned(),
        ),
        // The server says which side is behind.
        Standing::Incompatible(words) if words.contains("update the phux server") => (
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

/// The one thing to do about `standing`, when there is one.
fn remedy(standing: &Standing, p: &Ink) -> Option<El> {
    Some(match standing {
        Standing::Starting | Standing::Connecting(_) | Standing::Opening => return None,
        Standing::NoSessions => action("New session", "placeholder-session", p, Desk::new_session),
        Standing::NoTerminals => action(
            "New terminal",
            "placeholder-terminal",
            p,
            Desk::new_terminal,
        ),
        Standing::Incompatible(_) => action("Check for updates", "placeholder-update", p, |d| {
            d.updater.check_now()
        }),
        Standing::NoCli | Standing::Unreachable(_) | Standing::Closed(_) => {
            action("Retry", "placeholder-retry", p, Desk::reconnect)
        }
    })
}

pub fn placeholder(desk: &Desk, p: &Ink) -> El {
    let standing = desk.standing();
    let (title, detail) = words(&standing, desk.machines.label());
    let failed = matches!(
        standing,
        Standing::Unreachable(_) | Standing::Incompatible(_)
    );
    let color = if failed { p.danger } else { p.dim };
    let buttons = row()
        .w_full()
        .center()
        .gap(8.0)
        .pt(6.0)
        .children(remedy(&standing, p))
        .child(action(
            "Other machines…",
            "placeholder-machines",
            p,
            Desk::toggle_machines,
        ));
    col()
        .gap(6.0)
        .items_center()
        .max_w(520.0)
        .child(text(title).font(TextStyle::new(15.0, 600.0)).color(color))
        .child(text(detail).font(LABEL).color(p.dim).wrap())
        .children((standing == Standing::NoCli).then(|| install_line(p)))
        .child(buttons)
}

/// Over panes that have stopped updating: what happened, and a way to act.
/// It floats, so it never resizes a terminal.
pub fn banner(desk: &Desk, p: &Ink) -> Option<El> {
    let (lost, why) = desk.banner()?;
    Some(
        col()
            .absolute(0.0, 44.0)
            .w_full()
            .items_center()
            .transition(rune::Transition::opacity())
            .child(
                row()
                    .key("reconnect-banner")
                    .w(420.0)
                    .px(14.0)
                    .py(8.0)
                    .gap(12.0)
                    .radius(9.0)
                    .items_center()
                    .fill(p.raised)
                    .border(1.0, p.separator)
                    .child(
                        col()
                            .gap(2.0)
                            .grow(1.0)
                            .min_w(0.0)
                            .child(text(lost).font(LABEL_STRONG).color(p.text).truncate())
                            .child(text(why).font(CAPTION).color(p.dim).truncate()),
                    )
                    .child(action("Retry", "banner-retry", p, Desk::reconnect)),
            ),
    )
}

pub fn status_line(desk: &Desk, p: &Ink) -> El {
    let (word, color) = match desk.status {
        _ if desk.client.is_none() && desk.link_word() != "connecting" => ("not connected", p.dim),
        Status::Attached => ("attached", p.accent),
        Status::Idle | Status::Connecting => ("connecting", p.dim),
        Status::Negotiated => ("negotiated", p.dim),
        Status::Closed => ("closed", p.dim),
        Status::Failed => ("failed", p.danger),
    };
    let geometry = desk
        .client
        .as_ref()
        .zip(desk.shown.as_ref())
        .and_then(|(client, shown)| client.acquire_view(shown.view))
        .map(|frame| format!("{}×{}", frame.cols, frame.rows))
        .unwrap_or_default();
    let session = desk.selected_session_name().unwrap_or_default();
    row()
        .w_full()
        .h(24.0)
        .px(10.0)
        .gap(14.0)
        .items_center()
        .child(text(word).font(CAPTION).color(color))
        .child(text(session).font(CAPTION).color(p.dim).truncate())
        .child(el().grow(1.0))
        .child(match &desk.pointer.hover {
            // Where a ⌘-click goes, as a browser shows it.
            Some((_, link)) => text(link.url.clone())
                .font(CAPTION)
                .color(p.accent)
                .truncate(),
            None => notice_line(desk.notice.as_ref(), p),
        })
        .children(super::updates::pill(desk, p))
        .child(
            text(geometry)
                .font(CAPTION.family(Family::SystemMono))
                .color(p.dim),
        )
        .child(
            text(format!("{:.0} pt", desk.font_size))
                .font(CAPTION)
                .color(p.dim),
        )
}
