//! `[restore] scrollback`: hand the primary screen's text to the host once
//! output settles, and when the actor stops.

use std::time::{Duration, Instant};

use phux_terminal_api::{KEPT_SCROLLBACK_MAX_BYTES, KeepScrollback};

use super::TerminalActor;
use crate::grid::SnapshotSynthesizer;

/// How often an owed save is checked.
pub(super) const POLL: Duration = Duration::from_secs(1);
/// Output this old has settled.
const QUIET: Duration = Duration::from_secs(2);
/// Output that never settles is still saved this often.
const MAX_DELAY: Duration = Duration::from_secs(10);

/// What the host asked to keep, and the save it is owed.
#[derive(Debug)]
pub(super) struct Keep {
    ask: KeepScrollback,
    /// The first and the last output not saved yet.
    owed: Option<(Instant, Instant)>,
}

impl Keep {
    /// Output arrived at `now`.
    pub(super) const fn touch(&mut self, now: Instant) {
        let first = match self.owed {
            Some((first, _)) => first,
            None => now,
        };
        self.owed = Some((first, now));
    }

    fn due(&self, now: Instant) -> bool {
        self.owed.is_some_and(|(first, last)| {
            now.saturating_duration_since(last) >= QUIET
                || now.saturating_duration_since(first) >= MAX_DELAY
        })
    }
}

impl TerminalActor {
    /// Save this pane for `ask`, starting with what it already shows.
    pub(crate) fn keep_scrollback(&mut self, ask: KeepScrollback) {
        let mut keep = Keep { ask, owed: None };
        keep.touch(Instant::now());
        self.keep = Some(keep);
    }

    /// Whether a save is owed.
    pub(super) fn keep_owed(&self) -> bool {
        self.keep.as_ref().is_some_and(|keep| keep.owed.is_some())
    }

    /// Save when one is due.
    pub(super) fn service_keep(&mut self) {
        if self
            .keep
            .as_ref()
            .is_some_and(|keep| keep.due(Instant::now()))
        {
            self.keep_now();
        }
    }

    /// Save now if one is owed. The alternate screen reports nothing, which
    /// leaves the last save of the primary one standing.
    pub(super) fn keep_now(&mut self) {
        let Some(keep) = &mut self.keep else { return };
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            return;
        };
        if keep.owed.take().is_none() {
            return;
        }
        match SnapshotSynthesizer::kept_text(terminal, keep.ask.lines, KEPT_SCROLLBACK_MAX_BYTES) {
            Ok(Some(text)) => (keep.ask.save)(text),
            Ok(None) => {}
            Err(err) => tracing::warn!(error = %err, "scrollback not saved"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    fn kept(lines: u32) -> (TerminalActor, Rc<RefCell<Vec<String>>>) {
        let mut actor = TerminalActor::new(20, 5).expect("new").actor;
        let saves = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&saves);
        actor.keep_scrollback(KeepScrollback {
            lines,
            save: Rc::new(move |text| {
                sink.borrow_mut()
                    .push(String::from_utf8(text).expect("UTF-8"));
            }),
        });
        (actor, saves)
    }

    #[test]
    fn output_is_saved_once_quiet_or_after_the_cap() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut keep = kept(10).0.keep.take().expect("keep");
        keep.owed = None;
        assert!(!keep.due(at(60)), "nothing owed");
        keep.touch(at(0));
        assert!(!keep.due(at(1)) && keep.due(at(2)));
        for secs in 1..10 {
            keep.touch(at(secs));
            assert!(!keep.due(at(secs)));
        }
        keep.touch(at(10));
        assert!(keep.due(at(10)), "never quiet");
    }

    #[test]
    fn the_primary_screen_and_its_history_are_saved_and_the_alternate_never_is() {
        let (mut actor, saves) = kept(3);
        for line in 0..20 {
            actor.feed_for_test(format!("line {line}\r\n").as_bytes());
        }
        actor.feed_for_test(b"\x1b[31mred\x1b[0m");
        actor.keep_now();
        actor.keep_now();
        assert_eq!(saves.borrow().len(), 1, "saved only when owed");
        let text = saves.borrow()[0].clone();
        assert!(text.contains("line 19") && text.contains("red"), "{text:?}");
        assert!(text.contains("\x1b["), "styled: {text:?}");
        assert!(!text.contains("line 12\r"), "older than 3 rows: {text:?}");

        actor.feed_for_test(b"\x1b[?1049hSECRET");
        actor.keep_now();
        assert_eq!(saves.borrow().len(), 1, "alternate screen");
        actor.feed_for_test(b"\x1b[?1049l");
        actor.keep_now();
        assert!(!saves.borrow()[1].contains("SECRET"));
        assert!(saves.borrow()[1].contains("red"));

        actor.feed_for_test(b"\x1b[H\x1b[2J\x1b[3J");
        actor.keep_now();
        assert_eq!(saves.borrow()[2].trim(), "", "a clear is saved");
    }
}
