//! `notifications.from-programs = "notes"`: OSC 9 and OSC 777 from a pane
//! become its `phux.note/v1` record, the one `phux notify` writes. Off by
//! default, so no escape sequence a pane prints raises a notification.

use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{RESOURCE_NOTE_KEY, Scope};
use phux_terminal_api::agent_detect::ProgramNote;
use tokio::sync::watch;

use crate::state::SharedState;

/// At most one note per pane this often. The engine keeps only the newest
/// notification, so a burst lands as its last word, not as a queue.
const MIN_GAP: Duration = Duration::from_secs(1);

/// Longest title and body, in characters, so a note fits the reader's cap
/// (`phux_client_core::note::MAX_NOTE_BYTES`) even when every character
/// is escaped.
const MAX_TITLE_CHARS: usize = 120;
const MAX_BODY_CHARS: usize = 480;

/// Write each notification the pane's engine reports as its note, while
/// the server was started with `from-programs = "notes"`.
pub(in crate::runtime) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: watch::Receiver<Option<ProgramNote>>,
) {
    tokio::task::spawn_local(async move {
        while rx.changed().await.is_ok() {
            let note = rx.borrow_and_update().clone();
            let wrote = state.with_mut(|s| {
                if s.config().from_programs != phux_config::FromPrograms::Notes {
                    return false;
                }
                let Some(value) = note.as_ref().and_then(|n| note_json(n, now_ms())) else {
                    return false;
                };
                s.metadata_set(&Scope::Resource(terminal.clone()), RESOURCE_NOTE_KEY, value);
                true
            });
            if wrote {
                tokio::time::sleep(MIN_GAP).await;
            }
        }
    });
}

/// The `phux.note/v1` value for `note`, level `info`: OSC 9 carries only a
/// body, so it becomes the title. `None` when there is nothing to show.
fn note_json(note: &ProgramNote, at: u64) -> Option<Vec<u8>> {
    let title = clip(&note.title, MAX_TITLE_CHARS);
    let body = clip(&note.body, MAX_BODY_CHARS);
    let (title, body) = if title.is_empty() {
        (clip(&body, MAX_TITLE_CHARS), String::new())
    } else {
        (title, body)
    };
    if title.is_empty() {
        return None;
    }
    let mut value = serde_json::json!({ "level": "info", "title": title, "at": at });
    if !body.is_empty() {
        value["body"] = body.into();
    }
    serde_json::to_vec(&value).ok()
}

/// `text` on one line, trimmed, at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().chars().take(max).collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use phux_client_core::note::{MAX_NOTE_BYTES, NoteLevel, parse_note};

    use super::*;

    fn note(title: &str, body: &str) -> ProgramNote {
        ProgramNote {
            title: title.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn clients_read_what_osc_9_and_777_become() {
        let read = |n: &ProgramNote| note_json(n, 7).and_then(|v| parse_note(&v));
        let osc9 = read(&note("", "formatted 3 files")).expect("osc 9");
        assert_eq!(
            (osc9.level, osc9.title.as_str(), osc9.body, osc9.at),
            (NoteLevel::Info, "formatted 3 files", None, 7)
        );
        let osc777 = read(&note("build", "failed\nbadly")).expect("osc 777");
        assert_eq!(osc777.body.as_deref(), Some("failed badly"));
        assert_eq!(note_json(&note(" ", "\t"), 7), None);
        let worst = "\u{1}".repeat(4096);
        let huge = note_json(&note(&"\"".repeat(4096), &worst), 7).expect("clipped");
        assert!(huge.len() <= MAX_NOTE_BYTES);
        assert!(parse_note(&huge).is_some());
    }
}
