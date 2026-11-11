//! Native JSON text selection, separate from terminal grid selection.

use super::{Body, Desk, text_key};
use rune::Tx;

impl Desk {
    pub(in crate::desk) fn document_text_key(&self) -> Option<String> {
        let shown = self.shown.as_ref()?;
        let document = self.resources.entries.get(&shown.resource)?;
        matches!(document.body, Body::Tree(_)).then(|| text_key(&shown.resource))
    }

    pub(in crate::desk) fn native_text_shortcut(&self, event: &rune::KeyEvent) -> bool {
        matches!(event.key, rune::Key::Command('a' | 'c'))
            && event.mods.cmd
            && !event.mods.shift
            && !event.mods.alt
            && self.document_text_key().is_some()
    }

    pub(in crate::desk) fn copy_document(&self, tx: &mut Tx) -> bool {
        let Some(key) = self.document_text_key() else {
            return false;
        };
        let text = tx
            .text_selection()
            .filter(|selection| {
                selection
                    .texts()
                    .iter()
                    .any(|t| t.key() == Some(key.as_str()))
            })
            .map(|selection| selection.text().to_owned());
        if let Some(text) = text {
            tx.copy(text);
        }
        true
    }
}
