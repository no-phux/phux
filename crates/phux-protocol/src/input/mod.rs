//! Wire-level input event types, mirroring libghostty-vt's input events with
//! matching discriminants (ADR-0006); encoding lives in [`crate::wire`].

pub mod focus;
pub mod key;
pub mod mouse;
pub mod paste;

use focus::FocusEvent;
use key::KeyEvent;
use mouse::MouseEvent;
use paste::PasteEvent;

/// Any client-to-server input atom, so one command (`ROUTE_INPUT`,
/// `APPLY_INPUT`) can carry one without a variant per atom.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InputEvent {
    /// A structured key event (`INPUT_KEY` — `docs/spec/input.md` §2).
    Key(KeyEvent),
    /// A structured mouse event (`INPUT_MOUSE` — `docs/spec/input.md` §3).
    Mouse(MouseEvent),
    /// A focus state change (`INPUT_FOCUS` — `docs/spec/input.md` §4).
    Focus(FocusEvent),
    /// A paste payload (`INPUT_PASTE` — `docs/spec/input.md` §5).
    Paste(PasteEvent),
}

impl InputEvent {
    /// A one-line log narration of the event's structure, never its secret
    /// payload (key text, pasted bytes) (ADR-0028).
    #[must_use]
    pub fn narrate(&self) -> String {
        match self {
            Self::Key(e) => {
                let action = e.action;
                let key = e.key;
                let mods = e.mods;
                let text_len = e.text.as_ref().map(String::len);
                format!("key {action:?} {key:?} mods={mods:?} text_len={text_len:?}")
            }
            Self::Mouse(e) => format!("mouse {e:?}"),
            Self::Focus(e) => format!("focus {e:?}"),
            Self::Paste(e) => {
                let trust = e.trust;
                let data_len = e.data.len();
                format!("paste {trust:?} data_len={data_len}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputEvent, focus, key, paste};

    const SECRET_TEXT: &str = "rm -rf / --no-preserve-root && PASSWORD=hunter2";
    const SECRET_PASTE: &[u8] = b"ssh-private-key-BEGIN-SUPER-SECRET";

    fn secret_key_event() -> InputEvent {
        InputEvent::Key(key::KeyEvent {
            action: key::KeyAction::Press,
            key: key::PhysicalKey::A,
            mods: key::ModSet::CTRL,
            consumed_mods: key::ModSet::empty(),
            composing: false,
            text: Some(SECRET_TEXT.to_owned()),
            unshifted_codepoint: Some(u32::from('a')),
        })
    }

    fn secret_paste_event() -> InputEvent {
        InputEvent::Paste(paste::PasteEvent {
            trust: paste::PasteTrust::Untrusted,
            data: SECRET_PASTE.to_vec(),
        })
    }

    /// `{:?}` (what server traces print) must not leak key text or pastes.
    #[test]
    fn input_event_debug_never_leaks_secret_payload() {
        let key_dbg = format!("{:?}", secret_key_event());
        assert!(
            !key_dbg.contains(SECRET_TEXT),
            "key Debug leaked: {key_dbg}"
        );
        assert!(key_dbg.contains("text_len"), "{key_dbg}");

        let paste_dbg = format!("{:?}", secret_paste_event());
        let leaked = String::from_utf8_lossy(SECRET_PASTE);
        assert!(
            !paste_dbg.contains(leaked.as_ref()),
            "paste Debug leaked: {paste_dbg}"
        );
        assert!(paste_dbg.contains("data_len"), "{paste_dbg}");
    }

    #[test]
    fn narrate_is_structural_and_redaction_safe() {
        let key_n = secret_key_event().narrate();
        assert!(key_n.starts_with("key "), "{key_n}");
        assert!(
            !key_n.contains(SECRET_TEXT),
            "narrate leaked key text: {key_n}"
        );

        let paste_n = secret_paste_event().narrate();
        assert!(paste_n.starts_with("paste "), "{paste_n}");
        let leaked = String::from_utf8_lossy(SECRET_PASTE);
        assert!(
            !paste_n.contains(leaked.as_ref()),
            "narrate leaked paste: {paste_n}"
        );

        let focus_n = InputEvent::Focus(focus::FocusEvent::Gained).narrate();
        assert!(focus_n.starts_with("focus "), "{focus_n}");
    }
}
