//! Phone destinations are navigation, not aliases for command search.

use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::ResourceId;
use rune::{Key, KeyEvent, Tx};

use super::touch::Feedback;
use super::{Desk, Soft};
use crate::desk::Pending;

pub(crate) mod native;
mod returns;

pub use returns::Return;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Sessions,
    Actions,
    Switcher,
}

/// Where the Sessions list opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reveal {
    /// Where it was left, this far down: a search keeps its place among
    /// its results. Read as it opens, before the list reports again.
    Kept(f32),
    /// Around the pane on screen.
    Current,
}

/// A list's scroll offset, reported by its scroller in every frame that
/// moves it: a live value, so scrolling runs no view.
#[derive(Clone)]
pub struct Offset(rune::Live<f32>);

impl Default for Offset {
    fn default() -> Self {
        Self(rune::Live::new(0.0))
    }
}

impl std::fmt::Debug for Offset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.get().fmt(f)
    }
}

impl Offset {
    pub fn get(&self) -> f32 {
        self.0.get()
    }

    /// The scroller's report: points down from the top.
    pub fn follow(&self) -> impl Fn(rune::ScrollPos) + 'static {
        let offset = self.0.clone();
        move |at: rune::ScrollPos| {
            offset.set(at.offset[1]);
        }
    }
}

/// What a long press on the Sessions page holds: a session or one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// A session, by id.
    Session(u32),
    /// A pane and the session it belongs to, by name.
    Pane(ResourceId, String),
}

/// The long-press menu: what it is about, and where the finger was (window
/// points), so it opens beside the row instead of somewhere else.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub held: Held,
    /// The held row's element key: the menu opens clear of that row.
    pub row: String,
    pub at: [f32; 2],
    /// The source that owned the row when the hold opened it.
    pub source: Option<String>,
    /// The native host has been asked to open its own menu for it, which
    /// may take a pick after the menu has gone. Rune draws none there.
    pub asked: bool,
}

/// One verb of the long-press menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Open,
    NewPane,
    Rename,
    Fold,
    Close,
}

impl Verb {
    /// The name a native host's menu item answers with.
    pub fn id(self) -> &'static str {
        match self {
            Verb::Open => "open",
            Verb::NewPane => "new-pane",
            Verb::Rename => "rename",
            Verb::Fold => "fold",
            Verb::Close => "close",
        }
    }

    pub fn from_id(id: &str) -> Option<Verb> {
        [
            Verb::Open,
            Verb::NewPane,
            Verb::Rename,
            Verb::Fold,
            Verb::Close,
        ]
        .into_iter()
        .find(|verb| verb.id() == id)
    }
}

impl Desk {
    pub(in crate::desk) fn switch_sessions(&mut self) {
        if self.phone.is_none() {
            return self.open_palette(crate::palette::Mode::Sessions);
        }
        self.close_palette();
        self.close_inbox();
        self.close_settings();
        self.open_phone_sessions();
    }

    pub fn phone_page(&self) -> Option<Page> {
        self.phone.as_ref()?.page
    }

    pub fn open_phone_sessions(&mut self) {
        self.open_phone_navigation(Page::Sessions);
    }

    pub fn open_phone_actions(&mut self) {
        self.open_phone_navigation(Page::Actions);
    }

    pub(super) fn open_phone_navigation(&mut self, page: Page) {
        if self.modal().is_some() {
            return;
        }
        self.close_phone_menu();
        if let Some(phone) = &mut self.phone {
            phone.reset_navigation();
            phone.release();
            phone.swipe.arrival = None;
            phone.page = Some(page);
            phone.switcher = None;
            phone.menu = None;
            phone.return_to = None;
            // The filter is the last visit's; its keyboard waits for a tap.
            phone.query.focused = false;
            if page == Page::Sessions {
                phone.reveal = Some(if phone.query.text.is_empty() {
                    Reveal::Current
                } else {
                    Reveal::Kept(phone.sessions_scroll.get())
                });
            }
        }
    }

    /// Close the page. Sessions keeps its filter and place for the next
    /// visit; only picking a row is done with them.
    pub fn close_phone_navigation(&mut self) {
        self.close_phone_menu();
        if let Some(phone) = &mut self.phone {
            phone.page = None;
            phone.switcher = None;
            phone.menu = None;
            phone.query.focused = false;
            phone.query.cancel_composition();
            phone.release();
        }
    }

    /// A Sessions row was picked: its search is done with, and the page
    /// goes. The next visit opens around the pane it picked.
    pub fn close_phone_sessions_picked(&mut self) {
        self.close_phone_navigation();
        if let Some(phone) = &mut self.phone {
            phone.query.clear();
        }
    }

    /// The Sessions list's first layout after opening: back where it was
    /// for a kept search, else just far enough to show the pane on screen
    /// (nowhere, when it already shows). At the large text sizes search
    /// scrolls with the list, so the list opens at its top, where search
    /// leads, rather than scroll it away.
    pub fn reveal_phone_sessions(&mut self, large_text: bool, tx: &mut Tx) {
        const LIST: &str = "phone-session-list";
        let Some(reveal) = self.phone.as_mut().and_then(|p| p.reveal.take()) else {
            // A later resize (the keyboard) changes nothing the view reads.
            tx.live_only();
            return;
        };
        match reveal {
            Reveal::Kept(kept) => tx.scroll_to_instant(LIST, 0.0, kept),
            Reveal::Current if large_text => {}
            Reveal::Current => {
                if let Some(row) = self.current_session_row() {
                    tx.scroll_into_view_in(LIST, row);
                    tx.instant();
                }
            }
        }
    }

    /// The row that says where you are: the current pane, or its session's
    /// header while that session is folded.
    fn current_session_row(&self) -> Option<String> {
        let shown = self.connection.shown.as_ref()?;
        let name = self.selected_session_name()?;
        let session = self.connection.topology.as_ref()?.session_named(&name)?.id;
        let folded = self
            .phone
            .as_ref()
            .is_some_and(|p| p.folded.contains(&session));
        Some(if folded {
            format!("phone-session-{session}")
        } else {
            format!("phone-session-pane-{}", shown.resource)
        })
    }

    /// Hiding search preserves the filter and the terminal's keyboard choice.
    pub fn blur_phone_search(&mut self, tx: &mut Tx) {
        if let Some(phone) = &mut self.phone {
            phone.query.focused = false;
            phone.query.cancel_composition();
            phone.release();
        }
        tx.blur();
        tx.instant();
    }

    /// Clear the filter in place, with the editor ready for its replacement.
    /// This changes no terminal keyboard preference or selected pane.
    pub fn clear_phone_search(&mut self, tx: &mut Tx) {
        let Some(phone) = &mut self.phone else { return };
        phone.query.clear();
        phone.query.focused = true;
        self.phone_search_edited(tx);
        tx.focus("phone-session-query", false);
    }

    /// A filter starts at its first result, not the old inventory's offset.
    /// Shared by drawn, hardware and UIKit editing.
    pub fn phone_search_edited(&mut self, tx: &mut Tx) {
        if self.phone_page() == Some(Page::Sessions) {
            tx.scroll_to_instant("phone-session-list", 0.0, 0.0);
            tx.instant();
        }
    }

    pub fn phone_navigation_key(&mut self, event: &KeyEvent) -> bool {
        // Escape closes a menu first, unless what it was about has gone and
        // nothing is drawn: then it closes the page, as it would have.
        let menu = self.phone.as_ref().and_then(|p| p.menu.as_ref());
        let drawn = |m: &&Menu| self.native.is_none() && self.held_exists(&m.held);
        if event.key == Key::Escape && menu.is_some_and(|m| drawn(&m)) {
            self.close_phone_menu();
            return true;
        }
        if event.key == Key::Escape {
            self.close_phone_navigation();
            return true;
        }
        if self.phone_page() == Some(Page::Switcher) {
            return self.switcher_key(&event.key);
        }
        false
    }

    pub(super) fn phone_navigation_soft(&mut self, soft: Soft, tx: &mut Tx) {
        let Some(phone) = &mut self.phone else { return };
        let event = crate::keys::plain(phone.for_field(soft));
        if self.phone_navigation_key(&event) {
            return;
        }
        if let Some(phone) = &mut self.phone
            && phone.query.focused
        {
            let before = phone.query.text.clone();
            let outcome = phone.query.key(&event, None);
            if phone.query.text != before {
                self.phone_search_edited(tx);
            }
            if outcome.submit || outcome.blur {
                self.blur_phone_search(tx);
            }
        }
    }

    pub fn phone_session(&mut self, name: &str) {
        self.close_phone_sessions_picked();
        self.show_session(name);
    }

    pub fn phone_create_session(&mut self) {
        let name = self
            .phone
            .as_ref()
            .map(|phone| phone.query.text.trim().to_owned());
        self.close_phone_sessions_picked();
        match name.filter(|name| !name.is_empty()) {
            Some(name) => self.create_session(name),
            None => self.new_session(),
        }
    }
}

// Folding and the long-press menu: the Sessions page's organisation.
impl Desk {
    /// Fold a session to its header, or unfold it.
    pub fn toggle_phone_fold(&mut self, session: u32) {
        let Some(phone) = &mut self.phone else { return };
        if !phone.folded.remove(&session) {
            phone.folded.insert(session);
        }
    }

    /// Show or fold the quiet sessions under their band.
    pub fn toggle_phone_quiet(&mut self) {
        if let Some(phone) = &mut self.phone {
            phone.quiet_shown = !phone.quiet_shown;
        }
    }

    /// A hold on a row: the menu opens where the finger is, with a tick.
    /// Search's keyboard goes, so the menu has the screen.
    pub fn open_phone_menu(&mut self, held: Held, row: String, at: [f32; 2], tx: &mut Tx) {
        if self.show_phone_menu(held, row, at, tx) {
            self.phone_feedback(Feedback::Hold);
        }
    }

    /// A session header's ⋯: the menu a hold opens, for a thumb that never
    /// thought to hold. It hangs from the ⋯ at the page's trailing edge.
    pub fn open_phone_more(&mut self, held: Held, row: String, tx: &mut Tx) {
        let edge = self.phone.as_ref().map_or(0.0, |p| p.page_size[0]);
        self.show_phone_menu(held, row, [edge, 0.0], tx);
    }

    /// The menu for `held`, beside `row`; whether it opened.
    fn show_phone_menu(&mut self, held: Held, row: String, at: [f32; 2], tx: &mut Tx) -> bool {
        let source = native::source(self, &held);
        self.close_phone_menu();
        let Some(phone) = &mut self.phone else {
            return false;
        };
        if phone.page != Some(Page::Sessions) {
            return false;
        }
        // Search's keyboard goes; a keyboard focus elsewhere stays, so the
        // modal hands it back when the menu closes.
        if phone.query.focused {
            phone.query.focused = false;
            phone.query.cancel_composition();
            tx.blur();
        }
        phone.menu = Some(Menu {
            held,
            row,
            at,
            source,
            asked: false,
        });
        true
    }

    /// Whether what a menu was opened on is still listed.
    pub fn held_exists(&self, held: &Held) -> bool {
        match held {
            Held::Session(id) => self
                .connection
                .topology
                .as_ref()
                .is_some_and(|t| t.sessions.iter().any(|s| s.id == *id)),
            Held::Pane(id, _) => self.addressable().iter().any(|r| &r.id == id),
        }
    }

    pub fn close_phone_menu(&mut self) {
        if let Some(native) = &mut self.native {
            native.held_menu = None;
        }
        if let Some(phone) = &mut self.phone {
            phone.menu = None;
        }
    }

    /// Run a menu verb on what was held. A verb that leaves for the terminal
    /// or a sheet closes the page; a close that asks first shows its
    /// question over the terminal, where the confirm bar is.
    pub fn phone_menu_pick(&mut self, verb: Verb) {
        let Some(menu) = self.phone.as_mut().and_then(|phone| phone.menu.take()) else {
            return;
        };
        self.phone_held_pick(menu.held, verb);
    }

    pub(crate) fn phone_held_pick(&mut self, held: Held, verb: Verb) {
        // A question that arrived meanwhile is answered first, over the
        // terminal where its bar is; no verb replaces it.
        if self.pending.is_some() {
            return self.close_phone_navigation();
        }
        match held {
            Held::Session(id) => self.session_verb(id, verb),
            Held::Pane(id, session) => self.pane_verb(id, &session, verb),
        }
    }

    fn session_verb(&mut self, id: u32, verb: Verb) {
        let Some(session) = self
            .connection
            .topology
            .as_ref()
            .and_then(|t| t.sessions.iter().find(|s| s.id == id))
            .map(|s| (s.name.clone(), s.key))
        else {
            return;
        };
        let (name, key) = session;
        match verb {
            Verb::Open => self.phone_session(&name),
            Verb::NewPane => self.phone_new_pane(id, None),
            Verb::Fold => self.toggle_phone_fold(id),
            Verb::Rename => {
                if let Some(key) = key {
                    self.close_phone_navigation();
                    self.rename_session(key, &name);
                }
            }
            Verb::Close => {
                if let Some(key) = key {
                    self.close_phone_navigation();
                    self.close_session_key(key);
                }
            }
        }
    }

    fn pane_verb(&mut self, id: ResourceId, session: &str, verb: Verb) {
        match verb {
            Verb::Open => {
                self.close_phone_sessions_picked();
                self.show(id, session);
            }
            Verb::NewPane => {
                let topology = self.connection.topology.as_ref();
                let pane = topology.and_then(|t| t.pane(&id));
                let cwd = pane.and_then(|p| p.cwd.clone()).filter(|c| !c.is_empty());
                let owner = topology
                    .and_then(|t| t.sessions.iter().find(|s| s.name == session))
                    .map(|s| s.id);
                if let Some(owner) = owner {
                    self.phone_new_pane(owner, cwd);
                }
            }
            Verb::Close => {
                self.close_resource(id.clone());
                // A busy terminal asks first; the question is about the pane
                // on screen, so go to it.
                if matches!(&self.pending, Some(Pending::Close(asked)) if *asked == id) {
                    self.close_phone_navigation();
                    self.show(id, session);
                }
            }
            Verb::Rename | Verb::Fold => {}
        }
    }

    /// A new terminal in `session`, in `cwd` or the session's first pane's
    /// folder. It opens in place of the page once the server makes it.
    fn phone_new_pane(&mut self, session: u32, cwd: Option<String>) {
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let cwd = cwd.or_else(|| {
            let topology = self.connection.topology.as_ref()?;
            self.addressable()
                .iter()
                .filter(|r| r.session_id == session)
                .find_map(|r| topology.pane(&r.id)?.cwd.clone())
                .filter(|c| !c.is_empty())
        });
        if let Some(name) = self
            .connection
            .topology
            .as_ref()
            .and_then(|t| t.sessions.iter().find(|s| s.id == session))
            .map(|s| s.name.clone())
        {
            self.phone_session(&name);
        }
        let initial_size = self.tab_spawn_size();
        if let Some(request) = self.spawn_request(
            &client,
            SpawnRequest {
                command: None,
                cwd,
                env: None,
                session_id: Some(session),
                initial_size,
            },
        ) {
            self.connection.pending_spawns.push(request);
        }
    }
}
