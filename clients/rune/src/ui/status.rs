//! The window's words about itself: the status line, the bar an action
//! waits in, and what stands in for a terminal that isn't there.

use phux_client_runtime::control::Status;
use rune::{Color, El, Family, TextStyle, col, el, row, text};

use super::{
    CAPTION, Ink, LABEL, LABEL_STRONG, TITLEBAR_H, button, icon_press, icons, keep_from_pane,
    pressable, tabs,
};
use crate::desk::link::{Banner, Standing};
use crate::desk::{Desk, Notice, NoticeKind, Pending};
use crate::launch::INSTALL;

/// A notice's words, wrapping; with none it draws nothing.
pub fn notice_line(notice: Option<&Notice>, p: &Ink) -> El {
    let (line, kind) = notice.map_or((String::new(), NoticeKind::Error), |notice| {
        (notice.text.clone(), notice.kind)
    });
    let color = match kind {
        NoticeKind::Info => p.text,
        NoticeKind::Error => p.danger,
    };
    text(line)
        .font(CAPTION)
        .color(color)
        .wrap()
        .grow(1.0)
        .min_w(0.0)
}

/// What a held action would do, and the word for doing it.
fn question(pending: &Pending) -> (String, &'static str) {
    match pending {
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
        Pending::CloseTab(terminals) if terminals.len() == 1 => (
            "Close this tab? Anything running in it ends.".to_owned(),
            "Close (Return)",
        ),
        Pending::CloseTab(terminals) => (
            format!(
                "Close this tab and its {} terminals? Anything running in them ends.",
                terminals.len()
            ),
            "Close (Return)",
        ),
        Pending::CloseSession(_, name) => (
            format!("End session {name} and everything running in it?"),
            "End session (Return)",
        ),
    }
}

/// The action waiting on Return or Escape, floating over the terminals so
/// asking never resizes them.
pub fn confirm_bar(desk: &Desk, p: &Ink) -> Option<El> {
    let (message, verb) = question(desk.pending.as_ref()?);
    Some(
        row()
            .key("confirm-bar")
            .on_drag(keep_from_pane)
            .pin(Some(0.0), Some(0.0), Some(0.0), None)
            .px(12.0)
            .py(8.0)
            .gap(8.0)
            .items_center()
            .fill(p.raised)
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
    match standing {
        Standing::Starting => ("Starting phux…", String::new()),
        Standing::NoCli => (
            "phux isn't installed",
            "This app runs on the phux command-line tool. Install it, and this window connects \
             on its own."
                .to_owned(),
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
fn remedy(standing: &Standing, remote: bool, p: &Ink) -> Option<El> {
    Some(match standing {
        Standing::Starting | Standing::Connecting(_) | Standing::Opening => return None,
        Standing::NoSessions => action("New session", "placeholder-session", p, Desk::new_session),
        Standing::NoTerminals => action(
            "New terminal",
            "placeholder-terminal",
            p,
            Desk::new_terminal,
        ),
        Standing::Incompatible(_) if updates_can_fix(standing, remote) => {
            action("Check for updates", "placeholder-update", p, |d| {
                d.updater.check_now();
            })
        }
        Standing::Incompatible(_)
        | Standing::NoCli
        | Standing::Unreachable(_)
        | Standing::Closed(_) => retry(p),
    })
}

fn retry(p: &Ink) -> El {
    action("Retry", "placeholder-retry", p, Desk::reconnect)
}

pub fn placeholder(desk: &Desk, p: &Ink) -> El {
    let standing = desk.standing();
    let remote = desk.machines.current.is_some();
    let (title, detail) = words(&standing, desk.machines.label(), remote);
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
        .children(remedy(&standing, remote, p))
        // A server updated from elsewhere never says so.
        .children(updates_can_fix(&standing, remote).then(|| retry(p)))
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
    let banner = desk.banner()?;
    let machine = desk.machines.label();
    let remote = desk.machines.current.is_some();
    let (lost, why) = match &banner {
        Banner::Reconnecting(why) => (format!("Lost {machine}. Reconnecting…"), why.clone()),
        Banner::Stuck(standing) => {
            let (title, why) = words(standing, machine, remote);
            (title.to_owned(), why)
        }
    };
    let update = matches!(&banner, Banner::Stuck(standing) if updates_can_fix(standing, remote));
    // A held confirmation keeps the top of the stage.
    let top = if desk.pending.is_some() { 96.0 } else { 44.0 };
    Some(
        col()
            .absolute(0.0, top)
            .w_full()
            .items_center()
            .transition(rune::Transition::opacity())
            .child(
                row()
                    .key("reconnect-banner")
                    .on_drag(keep_from_pane)
                    .w(if update { 560.0 } else { 420.0 })
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
                            .child(text(why).font(CAPTION).color(p.dim).wrap()),
                    )
                    .children(update.then(|| {
                        action("Check for updates", "banner-update", p, |d| {
                            d.updater.check_now();
                        })
                    }))
                    .child(action("Retry", "banner-retry", p, Desk::reconnect)),
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
    let terminal = (title != session && !title.is_empty())
        .then(|| text(title).font(LABEL).color(p.dim).truncate());
    row()
        .gap(7.0)
        .min_w(0.0)
        .shrink()
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
        .and_then(|(client, shown)| client.acquire_view(shown.view))
        .map(|frame| format!("{}×{}", frame.cols, frame.rows))
        .unwrap_or_default();
    let folded = !desk.sidebar_open;
    row()
        .key("titlebar")
        .w_full()
        .h(TITLEBAR_H)
        .pl(if folded { lights + 8.0 } else { 2.0 })
        .pr(12.0)
        .gap(8.0)
        .items_center()
        .window_drag()
        .child(icon_press(
            &icons::SIDEBAR,
            "sidebar-toggle",
            p,
            Desk::toggle_sidebar,
        ))
        .children(tabs::strip(desk, p).or_else(|| Some(place(desk, p))))
        .child(el().grow(1.0))
        // Where a ⌘-click goes, as a browser shows it.
        .children(desk.pointer.hover.as_ref().map(|(_, link)| {
            text(link.url.clone())
                .font(CAPTION)
                .color(p.accent)
                .truncate()
        }))
        .children(link_trouble(desk, p).map(|(word, color)| text(word).font(CAPTION).color(color)))
        .children(super::updates::pill(desk, p))
        .child(
            text(geometry)
                .font(CAPTION.family(Family::SystemMono))
                .color(p.dim),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::ResourceId;

    #[test]
    fn a_remote_servers_update_happens_on_that_machine() {
        let behind =
            Standing::Incompatible("server requires 12.x; update the phux server".to_owned());
        assert!(updates_can_fix(&behind, false));
        assert!(!updates_can_fix(&behind, true));
        assert!(
            words(&behind, "mini", true)
                .1
                .contains("`phux update` on mini")
        );
        assert!(!words(&behind, "here", false).1.contains("`phux update`"));
        // The app is this computer's to update, wherever the server is.
        let ahead = Standing::Incompatible("update the phux app/client".to_owned());
        assert!(updates_can_fix(&ahead, true));
    }

    #[test]
    fn a_held_tab_close_counts_its_terminals() {
        let close = |n| Pending::CloseTab((0..n).map(ResourceId::local).collect());
        assert_eq!(
            question(&close(1)).0,
            "Close this tab? Anything running in it ends."
        );
        assert_eq!(
            question(&close(3)).0,
            "Close this tab and its 3 terminals? Anything running in them ends."
        );
    }
}
