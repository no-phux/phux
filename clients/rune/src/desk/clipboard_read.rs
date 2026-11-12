//! OSC 52 clipboard reads (`docs/program-sequences.md#clipboard-osc-52`).
//!
//! `defaults.clipboard-read` in `config.toml` decides for this window:
//! `deny` (the default) drops every read, `allow` answers with this
//! machine's clipboard, `ask` holds the read as a question (Return allows
//! once, A allows that terminal for as long as the window is open, Escape
//! denies). Only the terminal with the keyboard, in a focused window, may
//! read, and one terminal asks at most once per [`PROMPT_COOLDOWN`]. This
//! machine has no primary selection, so a read of `p` is never answered.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use phux_client_core::engine::ClipboardSelection;
use phux_config_core::ClipboardRead;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, Pending};

/// The shortest time between two questions from one terminal.
pub const PROMPT_COOLDOWN: Duration = Duration::from_secs(3);

/// This window's grants and recent questions.
#[derive(Default)]
pub struct ClipboardReads {
    /// `defaults.clipboard-read`, read from `config.toml` at the first read.
    policy: Option<ClipboardRead>,
    /// Terminals allowed for as long as the window is open.
    granted: HashSet<ResourceId>,
    /// When each terminal last asked.
    asked: HashMap<ResourceId, Instant>,
}

impl ClipboardReads {
    /// A window that applies `policy`, for tests.
    #[cfg(test)]
    pub(super) fn with_policy(policy: ClipboardRead) -> Self {
        Self {
            policy: Some(policy),
            ..Self::default()
        }
    }
}

/// `config.toml`'s `defaults.clipboard-read`; `deny` when the file does not
/// load. Tests never read the real file.
fn file_policy() -> ClipboardRead {
    if cfg!(test) {
        return ClipboardRead::Deny;
    }
    phux_config_core::loader::load_client()
        .map(|config| config.defaults.clipboard_read)
        .unwrap_or_default()
}

impl Desk {
    /// A program in `terminal` asked to read `selection`. True when the
    /// window changed (a question went up).
    pub(super) fn clipboard_read(
        &mut self,
        terminal: ResourceId,
        selection: ClipboardSelection,
        tx: &mut Tx,
    ) -> bool {
        if !self.terminal_has_keyboard(&terminal) || selection != ClipboardSelection::Clipboard {
            return false;
        }
        let reads = &mut self.resources.clipboard_reads;
        let policy = *reads.policy.get_or_insert_with(file_policy);
        let allowed = policy == ClipboardRead::Allow
            || (policy == ClipboardRead::Ask && reads.granted.contains(&terminal));
        if allowed {
            self.answer_clipboard_read(terminal, selection, tx);
            return false;
        }
        let now = Instant::now();
        let recently = reads
            .asked
            .get(&terminal)
            .is_some_and(|at| now.duration_since(*at) < PROMPT_COOLDOWN);
        if policy != ClipboardRead::Ask || recently || self.pending.is_some() {
            return false;
        }
        reads.asked.insert(terminal.clone(), now);
        self.pending = Some(Pending::ClipboardRead(terminal, selection));
        true
    }

    /// The person allowed a held read; `for_terminal` allows every later
    /// one from that terminal too.
    pub(super) fn allow_clipboard_read(
        &mut self,
        terminal: ResourceId,
        selection: ClipboardSelection,
        for_terminal: bool,
        tx: &mut Tx,
    ) {
        if for_terminal {
            self.resources
                .clipboard_reads
                .granted
                .insert(terminal.clone());
        }
        self.answer_clipboard_read(terminal, selection, tx);
    }

    /// The confirm bar's keys while a read is held: Return allows once, A
    /// allows the terminal, Escape denies. `None` when no read is held.
    pub(super) fn clipboard_read_key(
        &mut self,
        event: &rune::KeyEvent,
        tx: &mut Tx,
    ) -> Option<bool> {
        let Some(Pending::ClipboardRead(terminal, selection)) = &self.pending else {
            return None;
        };
        let (terminal, selection) = (terminal.clone(), *selection);
        let for_terminal = match &event.key {
            rune::Key::Enter => false,
            rune::Key::Text(a) if a.eq_ignore_ascii_case("a") => true,
            rune::Key::Escape => {
                self.pending = None;
                return Some(true);
            }
            _ => return Some(false),
        };
        self.pending = None;
        self.allow_clipboard_read(terminal, selection, for_terminal, tx);
        Some(true)
    }

    /// The confirm bar's button: [`Self::confirm_pending`], reading the
    /// clipboard for a held read.
    pub fn confirm_pending_tx(&mut self, tx: &mut Tx) {
        match self.pending.take() {
            Some(Pending::ClipboardRead(terminal, selection)) => {
                self.allow_clipboard_read(terminal, selection, false, tx);
            }
            other => {
                self.pending = other;
                self.confirm_pending();
            }
        }
    }

    /// Read this machine's clipboard and send it to `terminal`, if it still
    /// has the keyboard when the clipboard answers.
    fn answer_clipboard_read(
        &self,
        terminal: ResourceId,
        selection: ClipboardSelection,
        tx: &mut Tx,
    ) {
        tx.read_clipboard(move |desk: &mut Self, content, _| {
            if !desk.terminal_has_keyboard(&terminal) {
                return;
            }
            if let Some(client) = &desk.client {
                let _ =
                    client.send_clipboard_reply(&terminal, selection, content.text().to_owned());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use rune::{Env, Key, KeyEvent, Mods, Runtime};

    use super::*;
    use crate::launch::Resolved;

    const C: ClipboardSelection = ClipboardSelection::Clipboard;

    fn id(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    /// A window showing terminal 1, applying `policy`.
    fn window(policy: ClipboardRead) -> Runtime<Desk> {
        let mut desk = Desk::new(&Resolved::offline("clipboard reads"));
        desk.resources.clipboard_reads = ClipboardReads::with_policy(policy);
        desk.shown = Some(crate::desk::Shown {
            resource: id(1),
            view: phux_client_runtime::ViewId::from_raw(1),
        });
        desk.focused = true;
        Runtime::new(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0))
    }

    #[test]
    fn deny_a_background_terminal_a_hidden_window_and_primary_never_ask() {
        let mut rt = window(ClipboardRead::Ask);
        rt.update(|d, tx| {
            assert!(
                !d.clipboard_read(id(2), C, tx),
                "not the terminal on screen"
            );
            assert!(!d.clipboard_read(id(1), ClipboardSelection::Primary, tx));
            d.focused = false;
            assert!(!d.clipboard_read(id(1), C, tx), "window in the background");
        });
        assert_eq!(rt.state.pending, None);
        let mut rt = window(ClipboardRead::Deny);
        rt.update(|d, tx| assert!(!d.clipboard_read(id(1), C, tx)));
        assert_eq!(rt.state.pending, None);
    }

    #[test]
    fn ask_holds_the_read_and_a_terminal_asks_once_per_cooldown() {
        let mut rt = window(ClipboardRead::Ask);
        rt.update(|d, tx| assert!(d.clipboard_read(id(1), C, tx)));
        assert_eq!(rt.state.pending, Some(Pending::ClipboardRead(id(1), C)));
        rt.update(|d, tx| {
            let escape = KeyEvent {
                key: Key::Escape,
                mods: Mods::default(),
            };
            assert_eq!(d.clipboard_read_key(&escape, tx), Some(true));
            assert_eq!(d.pending, None, "denied");
            assert!(!d.clipboard_read(id(1), C, tx), "asked a moment ago");
        });
        assert_eq!(rt.state.pending, None);
    }

    #[test]
    fn a_allows_the_terminal_from_then_on() {
        let mut rt = window(ClipboardRead::Ask);
        rt.update(|d, tx| {
            assert!(d.clipboard_read(id(1), C, tx));
            let a = KeyEvent {
                key: Key::Text("a".into()),
                mods: Mods::default(),
            };
            assert_eq!(d.clipboard_read_key(&a, tx), Some(true));
            assert_eq!(d.pending, None);
            assert!(!d.clipboard_read(id(1), C, tx), "answered, not asked");
        });
        assert!(rt.state.resources.clipboard_reads.granted.contains(&id(1)));
        assert_eq!(rt.state.pending, None);
    }
}
