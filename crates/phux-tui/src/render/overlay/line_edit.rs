//! A single-line text field with the readline keys a shell user expects:
//! the shared editor behind the prompt modals and copy-mode search.
//!
//! `C-a`/`Home` and `C-e`/`End` jump to the ends, `C-b`/`C-f` and the arrows
//! move a character, `C-u` and `C-k` kill to the start or end, `C-w` kills
//! the word before the cursor, `C-h`/Backspace and `C-d`/Delete delete a
//! character. Text inserts at the cursor.

use phux_protocol::input::key::{KeyEvent, ModSet, PhysicalKey};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

/// A text buffer with a cursor at a char boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// Byte offset of the cursor, always on a char boundary.
    cursor: usize,
}

impl LineEdit {
    /// A field holding `initial`, the cursor at its end.
    #[must_use]
    pub fn new(initial: &str) -> Self {
        Self {
            text: initial.to_owned(),
            cursor: initial.len(),
        }
    }

    /// The current text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Insert `text` at the cursor, dropping control characters.
    pub fn insert(&mut self, text: &str) {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// Apply one pressed key. `true` when the key was a line edit (the
    /// caller then consumes it); Enter, Escape, and anything unknown are
    /// left to the caller.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if key.mods.contains(ModSet::CTRL) {
            return self.handle_ctrl(key.key);
        }
        match key.key {
            PhysicalKey::Backspace => self.delete_back(),
            PhysicalKey::Delete | PhysicalKey::NumpadDelete => self.delete_forward(),
            PhysicalKey::ArrowLeft => self.cursor = self.prev_boundary(),
            PhysicalKey::ArrowRight => self.cursor = self.next_boundary(),
            PhysicalKey::Home | PhysicalKey::NumpadHome => self.cursor = 0,
            PhysicalKey::End | PhysicalKey::NumpadEnd => self.cursor = self.text.len(),
            _ => match key.text.as_deref() {
                Some(text) if !text.chars().any(char::is_control) => self.insert(text),
                _ => return false,
            },
        }
        true
    }

    /// The readline control chords; any other `C-` key is not an edit.
    fn handle_ctrl(&mut self, key: PhysicalKey) -> bool {
        match key {
            PhysicalKey::A => self.cursor = 0,
            PhysicalKey::E => self.cursor = self.text.len(),
            PhysicalKey::B => self.cursor = self.prev_boundary(),
            PhysicalKey::F => self.cursor = self.next_boundary(),
            // BS (0x08) decodes as Ctrl+Backspace; it is also legacy C-h.
            PhysicalKey::H | PhysicalKey::Backspace => self.delete_back(),
            PhysicalKey::D => self.delete_forward(),
            PhysicalKey::U => {
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            PhysicalKey::K => self.text.truncate(self.cursor),
            PhysicalKey::W => self.delete_word_back(),
            _ => return false,
        }
        true
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn delete_back(&mut self) {
        let start = self.prev_boundary();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    fn delete_forward(&mut self) {
        let end = self.next_boundary();
        self.text.replace_range(self.cursor..end, "");
    }

    /// `C-w` (readline `unix-word-rubout`): whitespace before the cursor,
    /// then the non-whitespace run before that.
    fn delete_word_back(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(char::is_whitespace);
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(at, c)| at + c.len_utf8());
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// The text as spans with a reverse-video caret at the cursor (the host
    /// cursor is hidden while overlays paint).
    #[must_use]
    pub fn spans(&self, text_style: Style) -> Vec<Span<'static>> {
        let caret = Style::default().add_modifier(Modifier::REVERSED);
        let (before, rest) = self.text.split_at(self.cursor);
        let mut chars = rest.chars();
        let under = chars.next().map_or_else(|| " ".to_owned(), String::from);
        vec![
            Span::styled(before.to_owned(), text_style),
            Span::styled(under, text_style.patch(caret)),
            Span::styled(chars.as_str().to_owned(), text_style),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::input::key::KeyAction;

    fn key(key: PhysicalKey, mods: ModSet, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        }
    }

    fn ctrl(k: PhysicalKey) -> KeyEvent {
        key(k, ModSet::CTRL, None)
    }

    fn plain(k: PhysicalKey) -> KeyEvent {
        key(k, ModSet::empty(), None)
    }

    /// The field after `edits`, rendered with `|` at the cursor.
    fn after(initial: &str, edits: &[KeyEvent]) -> String {
        let mut field = LineEdit::new(initial);
        for edit in edits {
            assert!(field.handle_key(edit), "{edit:?} is an edit");
        }
        let mut shown = field.text.clone();
        shown.insert(field.cursor, '|');
        shown
    }

    #[test]
    fn readline_kills_cut_to_the_ends_and_the_previous_word() {
        assert_eq!(after("hello world", &[ctrl(PhysicalKey::U)]), "|");
        assert_eq!(
            after("hello world", &[ctrl(PhysicalKey::A), ctrl(PhysicalKey::K)]),
            "|"
        );
        assert_eq!(after("hello world", &[ctrl(PhysicalKey::W)]), "hello |");
        assert_eq!(after("hello world  ", &[ctrl(PhysicalKey::W)]), "hello |");
        assert_eq!(
            after(
                "one two three",
                &[ctrl(PhysicalKey::W), ctrl(PhysicalKey::W)]
            ),
            "one |"
        );
        let mid = [
            ctrl(PhysicalKey::A),
            ctrl(PhysicalKey::F),
            ctrl(PhysicalKey::F),
        ];
        let mut kill_start = mid.to_vec();
        kill_start.push(ctrl(PhysicalKey::U));
        assert_eq!(after("abcd", &kill_start), "|cd");
        let mut kill_end = mid.to_vec();
        kill_end.push(ctrl(PhysicalKey::K));
        assert_eq!(after("abcd", &kill_end), "ab|");
    }

    #[test]
    fn movement_and_deletion_respect_char_boundaries() {
        assert_eq!(after("héllo", &[ctrl(PhysicalKey::A)]), "|héllo");
        assert_eq!(
            after(
                "héllo",
                &[
                    plain(PhysicalKey::Home),
                    plain(PhysicalKey::ArrowRight),
                    plain(PhysicalKey::ArrowRight)
                ]
            ),
            "hé|llo"
        );
        assert_eq!(
            after(
                "héllo",
                &[
                    ctrl(PhysicalKey::A),
                    ctrl(PhysicalKey::F),
                    ctrl(PhysicalKey::D)
                ]
            ),
            "h|llo"
        );
        assert_eq!(
            after(
                "héllo",
                &[
                    plain(PhysicalKey::End),
                    ctrl(PhysicalKey::B),
                    ctrl(PhysicalKey::B),
                    plain(PhysicalKey::Backspace)
                ]
            ),
            "hé|lo"
        );
        assert_eq!(after("ab", &[ctrl(PhysicalKey::H)]), "a|");
        assert_eq!(after("ab", &[plain(PhysicalKey::Delete)]), "ab|");
        assert_eq!(
            after("", &[plain(PhysicalKey::Backspace), ctrl(PhysicalKey::W)]),
            "|"
        );
    }

    #[test]
    fn text_inserts_at_the_cursor_and_unknown_keys_pass_through() {
        let mut field = LineEdit::new("ac");
        assert!(field.handle_key(&plain(PhysicalKey::ArrowLeft)));
        assert!(field.handle_key(&key(PhysicalKey::B, ModSet::empty(), Some("b"))));
        field.insert("\u{1b}x\n");
        assert_eq!(field.as_str(), "abxc");

        assert!(!field.handle_key(&plain(PhysicalKey::Enter)));
        assert!(!field.handle_key(&plain(PhysicalKey::Escape)));
        assert!(!field.handle_key(&ctrl(PhysicalKey::R)));
        assert!(!field.handle_key(&key(PhysicalKey::Tab, ModSet::empty(), Some("\t"))));
        assert_eq!(field.as_str(), "abxc");
    }

    #[test]
    fn the_caret_covers_the_char_under_the_cursor_or_a_trailing_space() {
        let text: Vec<String> = LineEdit::new("ab")
            .spans(Style::default())
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(text, ["ab", " ", ""]);
        let mut field = LineEdit::new("ab");
        field.handle_key(&ctrl(PhysicalKey::A));
        let text: Vec<String> = field
            .spans(Style::default())
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(text, ["", "a", "b"]);
    }
}
