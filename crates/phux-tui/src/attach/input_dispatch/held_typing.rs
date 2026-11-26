//! Typing meant for a pane that is still opening.
//!
//! A pane the user chose opens once its attach answers. Until then what they
//! type is meant for it, so it reaches no other pane: not the one in view, one
//! that took focus when that closed or another client moved it, nor a floating
//! pane above. Anything else they do, a chord or a click, ends that.

use super::ctx::DispatchCtx;
use crate::attach::actions::Adopt;

impl DispatchCtx<'_> {
    /// Whether what is typed is meant for a pane still opening.
    pub(super) fn typing_held(&self) -> bool {
        let windows = self.pending_windows.values().map(|w| &w.adopt);
        let splits = self.pending_splits.values().map(|s| &s.adopt);
        windows
            .chain(splits)
            .any(|adopt| matches!(adopt, Some(Adopt::Existing { hold: true, .. })))
    }

    /// Hold typing for the panes still opening, or let it go.
    pub(super) fn hold_typing(&mut self, held: bool) {
        let windows = self.pending_windows.values_mut().map(|w| &mut w.adopt);
        let splits = self.pending_splits.values_mut().map(|s| &mut s.adopt);
        for adopt in windows.chain(splits) {
            if let Some(Adopt::Existing { hold, .. }) = adopt {
                *hold = held;
            }
        }
    }
}
