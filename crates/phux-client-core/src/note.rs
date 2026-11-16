//! The `phux.note/v1` record: the latest note a program posted for its pane
//! with `phux notify`. Each note replaces the last, so "90%" can become
//! "done" without stacking.
//!
//! Any local client can write the key, so a reader bounds its size, drops
//! control characters, and reads a newer level word as `info`.

/// The largest record a reader accepts.
pub const MAX_NOTE_BYTES: usize = 4 * 1024;

/// How much a note matters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteLevel {
    /// `warn`.
    Warn,
    /// `error`.
    Error,
    /// `info`, and any newer word.
    #[default]
    #[serde(other)]
    Info,
}

/// One note.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Note {
    /// How much it matters.
    #[serde(default)]
    pub level: NoteLevel,
    /// One line.
    pub title: String,
    /// More, when there is more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Unix milliseconds when it was posted, so the same words posted twice
    /// still change the value.
    #[serde(default)]
    pub at: u64,
}

/// The note `bytes` holds, or `None` when it holds no showable note.
#[must_use]
pub fn parse_note(bytes: &[u8]) -> Option<Note> {
    if bytes.len() > MAX_NOTE_BYTES {
        return None;
    }
    let mut note: Note = serde_json::from_slice(bytes).ok()?;
    note.title = clean(&note.title);
    note.body = note.body.as_deref().map(clean).filter(|b| !b.is_empty());
    (!note.title.is_empty()).then_some(note)
}

fn clean(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_phux_notify_writes_and_nothing_unsafe() {
        let note = Note {
            level: NoteLevel::Warn,
            title: "build".to_owned(),
            body: Some("90%".to_owned()),
            at: 1,
        };
        let bytes = serde_json::to_vec(&note).unwrap();
        assert_eq!(parse_note(&bytes), Some(note));
        let newer = parse_note(br#"{"level":"fatal","title":"t"}"#).unwrap();
        assert_eq!(newer.level, NoteLevel::Info);
        let dirty = parse_note(b"{\"title\":\"a\\u001b[2Jb\",\"body\":\"\\n\"}").unwrap();
        assert_eq!((dirty.title.as_str(), dirty.body), ("a [2Jb", None));
        assert_eq!(parse_note(br#"{"title":"  "}"#), None);
        assert_eq!(parse_note(b"not json"), None);
        let huge = format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_NOTE_BYTES));
        assert_eq!(parse_note(huge.as_bytes()), None);
    }
}
