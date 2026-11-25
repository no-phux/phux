//! A pane asks to open a link on this machine (`docs/ssh.md#opening-links`):
//! the prompt, and the answers the driver sends.
//!
//! The prompt is always shown, one at a time, and declining is the default.
//! Opening takes `o`, and only after [`QUIET`] with the prompt on screen and
//! no key pressed, so what was being typed into the pane never opens
//! anything.

use std::cell::{Cell, RefCell};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use phux_config::open_url::WebLink;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::open_url::OPEN_URL_SECS;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::render::overlay::widgets::{Modal, centered_panel, modal_inner_width, wrap_words};
use crate::render::overlay::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// How long the prompt must be on screen with no key pressed before a key
/// can answer it.
const QUIET: Duration = Duration::from_secs(1);
/// When the server takes the silence as a decline.
const DEADLINE: Duration = Duration::from_secs(OPEN_URL_SECS);
/// After a decline, how long further requests are declined unasked.
const COOLDOWN: Duration = Duration::from_secs(3);

/// Whether a link opened here reaches the person: not on the far side of an
/// ssh login or in another server's pane, and on a desktop.
pub(in crate::attach) fn opens_here() -> bool {
    let set = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    let elsewhere = ["SSH_CONNECTION", "SSH_TTY", "PHUX_TERMINAL_ID"];
    let desktop = cfg!(target_os = "macos") || set("DISPLAY") || set("WAYLAND_DISPLAY");
    desktop && !elsewhere.into_iter().any(set)
}

/// Hand `link` to the system's opener, as its one argument.
pub(in crate::attach) fn open(link: &WebLink) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(opener)
        .arg(link.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reaped on a thread, so it leaves no zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// One answer for the server: the link to open, or `None` to decline.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::attach) struct Answer {
    pub(in crate::attach) terminal: ResourceId,
    pub(in crate::attach) id: u64,
    pub(in crate::attach) link: Option<WebLink>,
}

/// The requests the driver and the prompt share.
#[derive(Debug, Default)]
pub(in crate::attach) struct OpenUrls {
    /// When the prompt on screen expires.
    expires: Option<Instant>,
    /// When a prompt was last declined.
    declined: Option<Instant>,
    /// Answers not sent yet.
    answers: Vec<Answer>,
}

pub(in crate::attach) type SharedOpenUrls = Rc<RefCell<OpenUrls>>;

impl OpenUrls {
    /// When the prompt on screen must go.
    pub(in crate::attach) const fn deadline(&self) -> Option<Instant> {
        self.expires
    }

    pub(in crate::attach) fn take_answers(&mut self) -> Vec<Answer> {
        std::mem::take(&mut self.answers)
    }

    /// The link to ask about. A bad one, and any while another is up or was
    /// just declined, is declined here.
    pub(in crate::attach) fn on_request(
        &mut self,
        terminal: &ResourceId,
        id: u64,
        url: &str,
        now: Instant,
    ) -> Option<WebLink> {
        let busy = self.expires.is_some_and(|at| now < at)
            || (self.declined).is_some_and(|at| now.duration_since(at) < COOLDOWN);
        let link = WebLink::parse(url).ok().filter(|_| !busy);
        if link.is_some() {
            self.expires = Some(now + DEADLINE);
        } else {
            self.answers.push(Answer {
                terminal: terminal.clone(),
                id,
                link: None,
            });
        }
        link
    }
}

/// The prompt. Dropped unanswered, it declines.
pub(in crate::attach) struct OpenUrlOverlay {
    terminal: ResourceId,
    id: u64,
    link: WebLink,
    who: String,
    note: Option<[String; 2]>,
    urls: SharedOpenUrls,
    expires: Instant,
    answered: bool,
    /// Since when it has been on screen with no key pressed.
    quiet: Cell<Option<Instant>>,
    /// The site's name fits on screen.
    legible: Cell<bool>,
    /// First row shown, and the rows the last paint had room for.
    top: usize,
    rows: Cell<(usize, usize)>,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl OpenUrlOverlay {
    /// Ask about `link` from `terminal`. `host` names the server's machine
    /// and `remote` is its name in `--remote`, when it has one.
    pub(in crate::attach) fn new(
        (terminal, id, link): (ResourceId, u64, WebLink),
        (host, session, remote): (&str, &str, Option<&str>),
        urls: SharedOpenUrls,
        now: Instant,
        theme: &Theme,
    ) -> Self {
        let pane = crate::attach::server_frame::pane_label(&terminal);
        let who = plain(&format!(
            "{pane} in {session} on {host} asks to open this link here:"
        ));
        let note = link.callback_port().zip(remote).map(|(port, remote)| {
            [
                format!("It comes back to port {port} on this machine. To pass that on:"),
                format!("phux forward -L {port}:localhost:{port} --remote {remote}"),
            ]
            .map(|line| plain(&line))
        });
        Self {
            terminal,
            id,
            link,
            who,
            note,
            urls,
            expires: now + DEADLINE,
            answered: false,
            quiet: Cell::new(None),
            legible: Cell::new(false),
            top: 0,
            rows: Cell::new((0, 0)),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    /// Every row at `width`, and how many of them, from the top, name who
    /// asks and the site.
    fn lines(&self, width: u16) -> (Vec<Line<'static>>, usize) {
        let width = usize::from(width).max(1);
        let text = Style::default().fg(self.theme.text);
        let who = wrap_words(&self.who, width);
        let site = chunks(self.link.host(), width);
        let named = who.len() + site.len();
        let note = self.note.iter().flat_map(|note| {
            let rows = note.iter().flat_map(|line| wrap_words(line, width));
            std::iter::once(String::new()).chain(rows)
        });
        let styled = |style| move |row: String| Line::from(Span::styled(row, style));
        let lines = (who.into_iter().map(styled(text)))
            .chain(
                site.into_iter()
                    .map(styled(text.add_modifier(Modifier::BOLD))),
            )
            .chain([Line::default()])
            .chain(
                chunks(self.link.as_str(), width)
                    .into_iter()
                    .map(styled(text)),
            )
            .chain(note.map(styled(Style::default().fg(self.theme.dim))))
            .collect();
        (lines, named)
    }

    fn area(&self, outer: Rect) -> Rect {
        let panel = centered_panel(outer, 7, 40, 6, self.breakpoints);
        let (lines, _) = self.lines(modal_inner_width(panel.width));
        let height = u16::try_from(lines.len() + 2)
            .unwrap_or(u16::MAX)
            .min(outer.height);
        let y = outer.y + (outer.height - height) / 2;
        Rect { y, height, ..panel }
    }

    fn answer(&mut self, open: bool, now: Instant) -> OverlayCommand {
        let open = open && now < self.expires;
        self.settle(open.then(|| self.link.clone()), now);
        OverlayCommand::Dismiss
    }

    fn settle(&mut self, link: Option<WebLink>, now: Instant) {
        if std::mem::replace(&mut self.answered, true) {
            return;
        }
        let mut urls = self.urls.borrow_mut();
        urls.expires = None;
        if link.is_none() {
            urls.declined = Some(now);
        }
        urls.answers.push(Answer {
            terminal: self.terminal.clone(),
            id: self.id,
            link,
        });
    }

    fn key_at(&mut self, key: &KeyEvent, now: Instant) -> OverlayCommand {
        if key.action == KeyAction::Release {
            return OverlayCommand::Stay;
        }
        let chord = ModSet::SHIFT | ModSet::CTRL | ModSet::ALT | ModSet::SUPER;
        let bare = !key.mods.intersects(chord) && key.action == KeyAction::Press;
        if bare && key.key == PhysicalKey::Escape {
            return self.answer(false, now);
        }
        // Any key starts the quiet again, so a person typing never gets there.
        let armed = self
            .quiet
            .get()
            .is_some_and(|at| now.duration_since(at) >= QUIET);
        self.quiet.set(self.quiet.get().map(|_| now));
        let (total, room) = self.rows.get();
        let last = total.saturating_sub(room);
        match key.key {
            PhysicalKey::ArrowDown => self.top = (self.top + 1).min(last),
            PhysicalKey::ArrowUp => self.top = self.top.saturating_sub(1),
            PhysicalKey::PageDown => self.top = (self.top + room).min(last),
            PhysicalKey::PageUp => self.top = self.top.saturating_sub(room),
            PhysicalKey::O if bare && armed && self.legible.get() => {
                return self.answer(true, now);
            }
            PhysicalKey::N | PhysicalKey::Enter if bare && armed => {
                return self.answer(false, now);
            }
            _ => {}
        }
        OverlayCommand::Stay
    }
}

impl Drop for OpenUrlOverlay {
    fn drop(&mut self) {
        self.settle(None, Instant::now());
    }
}

impl RenderOverlay for OpenUrlOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let area = self.area(area);
        let (lines, named) = self.lines(modal_inner_width(area.width));
        let total = lines.len();
        let room = usize::from(area.height.saturating_sub(2));
        let top = self.top.min(total.saturating_sub(room));
        self.rows.set((total, room));
        self.legible.set(named < room);
        self.quiet
            .set(self.quiet.get().or_else(|| Some(Instant::now())));
        let status = if total > room {
            format!("{}-{} of {total}", top + 1, (top + room).min(total))
        } else {
            String::new()
        };
        let shown = lines.into_iter().skip(top).take(room).collect();
        Modal::new(&self.theme, "Open a link?", shown)
            .footer(status, "↑↓ scroll · o open · esc decline")
            .render_into(area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        self.key_at(key, Instant::now())
    }

    fn handle_paste(&mut self, _text: &str) {
        self.quiet.set(self.quiet.get().map(|_| Instant::now()));
    }
}

/// `text` without what could hide or reorder it: names come from the server.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_ascii_graphic() || *c == ' ')
        .collect()
}

/// ASCII `text` cut every `width` columns.
fn chunks(text: &str, width: usize) -> Vec<String> {
    text.as_bytes()
        .chunks(width)
        .map(|row| String::from_utf8_lossy(row).into_owned())
        .collect()
}

#[cfg(test)]
mod tests;
