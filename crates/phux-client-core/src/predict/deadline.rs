//! When a guess stops being shown, for hosts that repaint on a timer.

use super::state::PredictionState;

impl PredictionState {
    /// The caller-clock millisecond after which the front guess stops being
    /// displayed; `None` with nothing pending. A host with no output to
    /// repaint on wakes then to erase a guess the program never echoed.
    #[must_use]
    pub fn display_deadline_ms(&self) -> Option<u64> {
        let front = self.pending().next()?;
        Some(front.queued_at_ms.saturating_add(self.display_ttl_ms()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::{PredictionState, PredictiveConfig};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    #[test]
    fn the_deadline_is_the_front_guess_plus_its_display_lifetime() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        assert_eq!(state.display_deadline_ms(), None);
        for (text, at) in [("a", 500), ("b", 900)] {
            let key = KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::Unidentified,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: Some(text.to_owned()),
                unshifted_codepoint: None,
            };
            let _ = state.predict_key_at(&key, at);
        }
        let deadline = state.display_deadline_ms().expect("a guess is queued");
        assert_eq!(deadline, 500 + state.display_ttl_ms());
        assert!(state.should_display(deadline));
        assert!(!state.should_display(deadline + 1));
    }
}
