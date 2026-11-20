//! OSC 52 clipboard writes (`docs/program-sequences.md#clipboard-osc-52`).
//!
//! `defaults.clipboard-write` in `config.toml` decides for this window:
//! `allow` (the default) sets this machine's clipboard, `deny` drops every
//! write, `ask` holds the write as a question (Return allows it, Escape
//! drops it; allowing waits out [`PROMPT_ARMING`]). Only the terminal with
//! the keyboard, in a focused window, may write, and a write that arrives
//! while another question is up is dropped. Copies the person makes in Rune
//! never come through here.

use std::time::Instant;

use phux_config_core::ClipboardWrite;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::clipboard_read::PROMPT_ARMING;
use super::{Desk, Pending};

/// This window's policy and its held write's question.
#[derive(Default)]
pub struct ClipboardWrites {
    /// `defaults.clipboard-write`, read from `config.toml` at the first write.
    policy: Option<ClipboardWrite>,
    /// When the held write's question went up.
    asked: Option<Instant>,
}

impl ClipboardWrites {
    /// A window that applies `policy`, for tests.
    #[cfg(test)]
    pub(super) fn with_policy(policy: ClipboardWrite) -> Self {
        Self {
            policy: Some(policy),
            ..Self::default()
        }
    }
}

/// `config.toml`'s `defaults.clipboard-write`; `deny` when the file does
/// not load, so a broken config never hands the clipboard to a program.
/// Tests never read the real file.
fn file_policy() -> ClipboardWrite {
    if cfg!(test) {
        return ClipboardWrite::Deny;
    }
    phux_config_core::loader::load_client().map_or(ClipboardWrite::Deny, |config| {
        config.defaults.clipboard_write
    })
}

impl Desk {
    /// A program in `terminal` asked to set the clipboard to `text`. True
    /// when the window changed (a question went up).
    pub(super) fn clipboard_write(
        &mut self,
        terminal: ResourceId,
        text: String,
        tx: &mut Tx,
    ) -> bool {
        if !self.terminal_has_keyboard(&terminal) {
            return false;
        }
        let writes = &mut self.connection.resources.clipboard_writes;
        match *writes.policy.get_or_insert_with(file_policy) {
            ClipboardWrite::Allow => {
                tx.copy(text);
                false
            }
            ClipboardWrite::Ask if self.pending.is_none() => {
                writes.asked = Some(Instant::now());
                self.pending = Some(Pending::ClipboardWrite(terminal, text));
                true
            }
            ClipboardWrite::Ask | ClipboardWrite::Deny => false,
        }
    }

    /// The confirm bar's keys while a write is held: Return allows it,
    /// Escape drops it. `None` when no write is held.
    pub(super) fn clipboard_write_key(
        &mut self,
        event: &rune::KeyEvent,
        tx: &mut Tx,
    ) -> Option<bool> {
        let Some(Pending::ClipboardWrite(..)) = &self.pending else {
            return None;
        };
        let armed = self
            .connection
            .resources
            .clipboard_writes
            .asked
            .is_none_or(|at| at.elapsed() >= PROMPT_ARMING);
        match &event.key {
            rune::Key::Escape => self.pending = None,
            rune::Key::Enter if armed => self.allow_clipboard_write(tx),
            _ => return Some(false),
        }
        Some(true)
    }

    /// Set the clipboard to the held write's text, if there is one.
    pub(super) fn allow_clipboard_write(&mut self, tx: &mut Tx) {
        if let Some(Pending::ClipboardWrite(_, text)) = self.pending.take() {
            tx.copy(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use rune::{Env, Key, Runtime};

    use super::*;
    use crate::launch::Resolved;

    fn id(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    /// A window showing terminal 1, applying `policy`.
    fn window(policy: ClipboardWrite) -> Runtime<Desk> {
        let mut desk = Desk::with_preferences(
            &Resolved::offline("clipboard writes"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.connection.resources.clipboard_writes = ClipboardWrites::with_policy(policy);
        desk.connection.shown = Some(crate::desk::Shown {
            resource: id(1),
            view: phux_client_runtime::ViewId::from_raw(1),
        });
        desk.focused = true;
        Runtime::new(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0))
    }

    #[test]
    fn deny_and_a_background_terminal_never_write_or_ask() {
        let mut rt = window(ClipboardWrite::Deny);
        rt.update(|d, tx| assert!(!d.clipboard_write(id(1), "x".into(), tx)));
        assert_eq!(rt.state.pending, None);
        let mut rt = window(ClipboardWrite::Ask);
        rt.update(|d, tx| {
            assert!(!d.clipboard_write(id(2), "x".into(), tx), "not on screen");
            d.focused = false;
            assert!(
                !d.clipboard_write(id(1), "x".into(), tx),
                "window in the background"
            );
        });
        assert_eq!(rt.state.pending, None);
    }

    #[test]
    fn ask_holds_the_write_until_return_and_escape_drops_it() {
        let mut rt = window(ClipboardWrite::Ask);
        rt.update(|d, tx| assert!(d.clipboard_write(id(1), "secret".into(), tx)));
        assert_eq!(
            rt.state.pending,
            Some(Pending::ClipboardWrite(id(1), "secret".into()))
        );
        rt.update(|d, tx| {
            assert!(
                !d.clipboard_write(id(1), "other".into(), tx),
                "one question at a time"
            );
            let enter = crate::keys::plain(Key::Enter);
            // Typed straight after it appears, Return does nothing.
            assert_eq!(d.clipboard_write_key(&enter, tx), Some(false));
            assert!(d.pending.is_some(), "still asking");
            let escape = crate::keys::plain(Key::Escape);
            assert_eq!(d.clipboard_write_key(&escape, tx), Some(true));
        });
        assert_eq!(rt.state.pending, None);
        rt.update(|d, tx| {
            assert!(d.clipboard_write(id(1), "again".into(), tx));
            d.connection.resources.clipboard_writes.asked = Some(Instant::now() - PROMPT_ARMING);
            let enter = crate::keys::plain(Key::Enter);
            assert_eq!(d.clipboard_write_key(&enter, tx), Some(true));
        });
        assert_eq!(rt.state.pending, None);
    }
}
