//! Feeding key presses to the resolver.

use super::super::ctx::DispatchCtx;

/// Result of feeding a key event through the resolver.
pub(in crate::attach) enum ChordOutcome {
    /// Chord extended a partial sequence; absorb and wait.
    Partial,
    /// Chord completed a binding; effects follow.
    Resolved(phux_config::keybind::ResolvedAction),
    /// An exclusive key mode ate a key it does not bind.
    Swallowed,
}

/// Feed a key press to the resolver (releases and repeats are ignored, so
/// held keys do not re-fire). `None` without a resolver or on no match.
pub(in crate::attach) fn consume_chord(
    ctx: &mut DispatchCtx<'_>,
    key_event: &phux_protocol::input::key::KeyEvent,
) -> Option<ChordOutcome> {
    use phux_protocol::input::key::{KeyAction, ModSet};
    let resolver = ctx.resolver.as_deref_mut()?;
    if !matches!(key_event.action, KeyAction::Press) {
        return None;
    }
    let chord = phux_config::keybind::KeyChord {
        // A kitty-mode host reports Caps/Num Lock on `CSI u` keys (Ctrl+A is
        // `CSI 97;69u` with Caps Lock on); bindings name held modifiers only.
        modifiers: key_event.mods - (ModSet::CAPS_LOCK | ModSet::NUM_LOCK),
        key: key_event.key,
    };
    match resolver.feed(chord) {
        phux_config::keybind::Feed::NoMatch => None,
        phux_config::keybind::Feed::Partial => {
            // Mid-chord: the user is partway through a multi-chord binding.
            // Debug (not info) — chord progress is finer-grained than the
            // resolved-action lifecycle event in `run_action`.
            tracing::debug!("chord: partial match, awaiting next chord");
            Some(ChordOutcome::Partial)
        }
        phux_config::keybind::Feed::Resolved(r) => {
            tracing::debug!(action = %r.action, "chord: resolved to action");
            Some(ChordOutcome::Resolved(r))
        }
        phux_config::keybind::Feed::Swallowed => {
            tracing::debug!("chord: swallowed by an exclusive key mode");
            Some(ChordOutcome::Swallowed)
        }
    }
}
