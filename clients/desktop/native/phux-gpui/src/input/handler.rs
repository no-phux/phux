//! The platform text input handler: IME composition, candidate bounds and
//! the platform paste.

use super::*;

impl EntityInputHandler for TerminalInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.admit_focused(window, cx, false).ok()?;
        let (bytes, adjusted) = self.composition.range(range)?;
        *actual = Some(adjusted);
        Some(self.composition.text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.admit_focused(window, cx, true).ok()?;
        Some(UTF16Selection {
            range: self.composition.selected.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.admit_focused(window, cx, false).ok()?;
        (!self.composition.text.is_empty()).then(|| 0..self.composition.len())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // insertText commits; unmark alone discards local preedit, never sends it.
        self.composition.clear();
        self.pending_key = None;
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.commit(range, text, window, cx);
        self.record(result);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_key = None;
        let result = self.admit_focused(window, cx, true).and_then(|_| {
            self.composition
                .mark(range, text, selected)
                .then_some(())
                .ok_or(InputError::InvalidRange)
        });
        self.record(result);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.admit_focused(window, cx, false).ok()?;
        self.composition.range(range)?;
        // The painter owns cursor geometry. A candidate window is anchored at
        // that cursor; no guessed width for emoji or shaped preedit is exposed.
        Some(self.metrics?.cursor_bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.admit_focused(window, cx, false).ok()?;
        self.metrics?.cursor_bounds.contains(&point).then_some(0)
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.admit_focused(window, cx, false).is_err() {
            return;
        }
        if let Some((_, range)) = self.composition.range(range) {
            self.composition.selected = range;
            cx.notify();
        }
    }

    fn text_length_utf16(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        self.admit_focused(window, cx, false).ok()?;
        Some(self.composition.len())
    }

    fn accepts_text_input(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.admit_focused(window, cx, true).is_ok()
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focused(window, cx);
        if let Some(notice) = self.paste_item(&item, focused, cx) {
            cx.emit(InputEvent::Notice(notice));
        }
        cx.notify();
    }
}
