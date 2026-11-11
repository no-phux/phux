//! UIKit offsets are UTF-16 code units; Rune edits byte ranges. Reject a
//! split surrogate and validate an entire candidate before touching state.
use super::Action;
use rune::{Ime, TextEdit};

pub(super) fn byte(text: &str, offset: usize) -> Result<usize, String> {
    let mut utf16 = 0;
    for (at, c) in text.char_indices() {
        if utf16 == offset {
            return Ok(at);
        }
        utf16 += c.len_utf16();
    }
    if utf16 == offset {
        Ok(text.len())
    } else {
        Err("Native selection is not a UTF-16 character boundary.".into())
    }
}

pub(super) fn edit(edit: &mut TextEdit, action: Action) -> Result<(), String> {
    match action {
        Action::Replace {
            expected,
            text,
            anchor,
            focus,
        } => {
            if edit.text != expected {
                return Err("Native text context is stale.".into());
            }
            let (a, f) = (byte(&text, anchor)?, byte(&text, focus)?);
            // Use Rune's own paste sanitizer and line policy, including its
            // CRLF handling. Map selection through the same prefix sanitizer.
            let normalized = sanitize(edit, &text);
            let a = sanitize(edit, &text[..a]).len();
            let f = sanitize(edit, &text[..f]).len();
            edit.undo_group(|edit| {
                edit.cancel_composition();
                if edit.text != normalized {
                    edit.set_selection(0, edit.text.len());
                    edit.paste(&normalized);
                }
                edit.set_selection(a, f);
            });
        }
        Action::Selection { anchor, focus } => {
            let (a, f) = (byte(&edit.text, anchor)?, byte(&edit.text, focus)?);
            edit.set_selection(a, f);
        }
        Action::Preedit {
            text,
            anchor,
            focus,
        } => {
            let (a, f) = (byte(&text, anchor)?, byte(&text, focus)?);
            edit.ime(&Ime::Preedit(text, Some((a, f))));
        }
        Action::Cancel => {
            edit.ime(&Ime::Preedit(String::new(), None));
        }
        Action::Paste { text } => {
            edit.paste(&text);
        }
        _ => return Err("This native operation does not address an editor.".into()),
    }
    Ok(())
}

fn sanitize(template: &TextEdit, text: &str) -> String {
    let mut probe = template.clone();
    probe.cancel_composition();
    probe.set_selection(0, probe.text.len());
    probe.paste(text);
    std::mem::take(&mut probe.text)
}
