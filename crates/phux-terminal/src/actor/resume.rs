//! The `resume=` key of OSC 7501: a token the program in the pane wants to
//! be relaunched with (`docs/program-protocol.md`, "Resume").
//!
//! The engine parses OSC 7501 but skips keys it does not know and reports
//! only fixed fields, so this reads `app` and `resume` out of the same PTY
//! bytes the engine is fed. The token goes to the server on its own
//! channel, never into a program-status record: every client can read
//! those.

// ponytail: a second parse of OSC 7501. Delete it for the engine's own
// report once libghostty exposes unknown keys.

use phux_core::ResumeToken;
use tokio::sync::watch;

const ESC: u8 = 0x1b;
/// What follows `ESC ]` in a report.
const INTRO: &[u8] = b"7501;";
/// The protocol's 4096-byte sequence limit, less `OSC 7501 ;` and `ST`.
const MAX_BODY: usize = 4087;

#[derive(Debug, Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    /// After `ESC`.
    Esc,
    /// After `ESC ]` and this many bytes of [`INTRO`].
    Intro(usize),
    Body,
    /// After `ESC` inside the body: `\` ends the report.
    BodyEsc,
}

/// What a write asks for: `Some(token)` hands one over, `None` withdraws it.
type Change = Option<ResumeToken>;

/// The scanner and the channel its findings leave on.
#[derive(Debug, Default)]
pub(super) struct Resume {
    state: State,
    body: Vec<u8>,
    sink: Option<watch::Sender<Change>>,
}

impl Resume {
    /// The server's end. It starts empty and only changes count, so a pane
    /// rebuilt by an upgrade keeps the token its record already holds.
    pub(super) fn subscribe(&mut self) -> watch::Receiver<Change> {
        let (tx, rx) = watch::channel(None);
        self.sink = Some(tx);
        rx
    }

    /// Scan PTY output about to be written to the engine and publish the
    /// last change it asks for.
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        if let Some(change) = self.scan(bytes) {
            self.publish(change);
        }
    }

    /// The program is gone (its shell is back at a prompt, or it exited),
    /// so nothing is left to resume. Sent even with no token seen here:
    /// after an upgrade or a relaunch the record may still hold one.
    pub(super) fn forget(&self) {
        self.publish(None);
    }

    fn publish(&self, change: Change) {
        if let Some(sink) = &self.sink {
            sink.send_replace(change);
        }
    }

    /// The last change `bytes` asks for, continuing a report an earlier
    /// write left open. Costs one `memchr` when no `ESC` is present.
    fn scan(&mut self, bytes: &[u8]) -> Option<Change> {
        let mut change = None;
        let mut rest = bytes;
        loop {
            if matches!(self.state, State::Ground) {
                let Some(at) = memchr::memchr(ESC, rest) else {
                    break;
                };
                rest = &rest[at..];
            }
            let Some((&byte, tail)) = rest.split_first() else {
                break;
            };
            rest = tail;
            self.state = match (self.state, byte) {
                (State::Body, 0x07) | (State::BodyEsc, b'\\') => {
                    change = self.report().or(change);
                    State::Ground
                }
                (State::Body, ESC) => State::BodyEsc,
                // CAN and SUB cancel the sequence; a longer one is not ours.
                (State::Body, 0x18 | 0x1a) => State::Ground,
                (State::Body, _) if self.body.len() == MAX_BODY => State::Ground,
                (State::Body, _) => {
                    self.body.push(byte);
                    State::Body
                }
                (State::Esc | State::BodyEsc, b']') => State::Intro(0),
                (State::Intro(seen), _) if INTRO.get(seen) == Some(&byte) => {
                    if seen + 1 == INTRO.len() {
                        self.body.clear();
                        State::Body
                    } else {
                        State::Intro(seen + 1)
                    }
                }
                (_, ESC) => State::Esc,
                _ => State::Ground,
            };
        }
        change
    }

    /// What the finished body asks for. An empty `resume=` withdraws the
    /// token. A token counts only with a well-formed `app` in the same
    /// report; anything else changes nothing.
    fn report(&self) -> Option<Change> {
        let body = std::str::from_utf8(&self.body).ok()?;
        let (mut app, mut resume) = (None, None);
        for pair in body.split(':') {
            match pair
                .split_once('=')
                .map(|(key, value)| (key.trim_ascii(), value.trim_ascii()))
            {
                Some(("app", value)) => app = Some(value),
                Some(("resume", value)) => resume = Some(value),
                _ => {}
            }
        }
        let resume = resume?;
        if resume.is_empty() {
            return Some(None);
        }
        ResumeToken::new(app?, resume).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Option<Change> {
        let mut resume = Resume::default();
        chunks
            .iter()
            .fold(None, |last, chunk| resume.scan(chunk).or(last))
    }

    fn token(app: &str, token: &str) -> Option<Change> {
        ResumeToken::new(app, token).map(Some)
    }

    #[test]
    fn a_report_split_at_any_byte_hands_over_the_same_token() {
        for report in [
            &b"out\x1b]7501;state=working:app=nvim:resume=abc123\x1b\\more"[..],
            b"out\x1b]7501;state=working:app=nvim:resume=abc123\x07more",
        ] {
            assert_eq!(scan(&[report]), token("nvim", "abc123"));
            for at in 0..=report.len() {
                assert_eq!(
                    scan(&[&report[..at], &report[at..]]),
                    token("nvim", "abc123"),
                    "split at {at}"
                );
            }
            let bytes: Vec<&[u8]> = report.chunks(1).collect();
            assert_eq!(scan(&bytes), token("nvim", "abc123"));
        }
    }

    #[test]
    fn the_last_pair_wins_and_a_token_needs_an_app_in_the_same_report() {
        let cases: [(&[u8], Option<Change>); 12] = [
            (
                b"\x1b]7501;app=a:resume=one:resume=two\x07",
                token("a", "two"),
            ),
            (b"\x1b]7501;app=a:app=b:resume=one\x07", token("b", "one")),
            (b"\x1b]7501; app = a : resume = one \x07", token("a", "one")),
            (b"\x1b]7501;state=working:resume=one\x07", None),
            (b"\x1b]7501;app=my app:resume=one\x07", None),
            (b"\x1b]7501;app=a:resume=-rf\x07", None),
            (b"\x1b]7501;app=a:resume=a b\x07", None),
            (b"\x1b]7501;app=a:resume=$(id)\x07", None),
            (b"\x1b]7501;state=working:app=a\x07", None),
            (b"\x1b]7501;?\x07", None),
            // An empty value withdraws the token, with or without an app.
            (b"\x1b]7501;state=idle:resume=\x07", Some(None)),
            (b"\x1b]7501;app=a:resume=one:resume=\x1b\\", Some(None)),
        ];
        for (bytes, want) in cases {
            assert_eq!(scan(&[bytes]), want, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn a_later_report_replaces_an_earlier_one_and_a_bad_one_changes_nothing() {
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x07\x1b]7501;app=a:resume=two\x07"]),
            token("a", "two")
        );
        assert_eq!(
            scan(&[
                b"\x1b]7501;app=a:resume=one\x07",
                b"\x1b]7501;app=a:resume=-x\x07"
            ]),
            token("a", "one")
        );
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x07\x1b]7501;resume=\x07"]),
            Some(None)
        );
    }

    #[test]
    fn other_sequences_and_cancelled_or_oversized_reports_are_ignored() {
        for bytes in [
            &b"plain text, no escapes"[..],
            b"\x1b[31mred\x1b[0m\x1b]0;app=a:resume=one\x07",
            b"\x1b]133;A\x07\x1b]75010;app=a:resume=one\x07",
            b"\x1b]7501app=a:resume=one\x07",
            b"]7501;app=a:resume=one\x07",
            b"\x1b]7501;app=a:resume=one\x18\x07",
            b"\x1b]7501;app=a:resume=one\x1a\x1b\\",
            // An `ESC` that is not `ST` abandons the report.
            b"\x1b]7501;app=a:resume=one\x1b[0m\x07",
            b"\x1b]7501;app=a:resume=one",
        ] {
            assert_eq!(scan(&[bytes]), None, "{:?}", String::from_utf8_lossy(bytes));
        }
        // An abandoned report does not hide the next one.
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x1b\x1b]7501;app=b:resume=two\x07"]),
            token("b", "two")
        );

        let report = |body_len: usize| {
            let mut bytes = b"\x1b]7501;app=a:resume=one:x=".to_vec();
            let pad = body_len - (bytes.len() - 7);
            bytes.extend(std::iter::repeat_n(b'a', pad));
            bytes.push(0x07);
            bytes
        };
        assert_eq!(scan(&[&report(MAX_BODY)]), token("a", "one"));
        assert_eq!(scan(&[&report(MAX_BODY + 1)]), None);
    }

    #[test]
    fn changes_leave_on_the_channel_and_forgetting_always_notifies() {
        let mut resume = Resume::default();
        let mut rx = resume.subscribe();
        assert!(!rx.has_changed().expect("open"));
        resume.observe(b"no report here");
        assert!(!rx.has_changed().expect("open"));
        resume.observe(b"\x1b]7501;state=working:app=a:resume=one\x07");
        assert_eq!(*rx.borrow_and_update(), ResumeToken::new("a", "one"));
        resume.forget();
        assert!(rx.has_changed().expect("open"));
        assert_eq!(*rx.borrow_and_update(), None);
        // A pane that never saw a token still tells the server to forget.
        resume.forget();
        assert!(rx.has_changed().expect("open"));
    }

    /// The engine skips the key, so the report is still a status report,
    /// and the shell's next prompt ends the program the token was for.
    #[test]
    fn the_engine_ignores_the_key_and_a_prompt_forgets_the_token() {
        use super::super::effects::{self, Effect};
        use super::super::program_status::Update;

        let report = b"\x1b]7501;state=working:app=a:resume=one\x1b\\";
        let (seen, _) = effects::observe(&[report]);
        assert!(
            matches!(&seen[..], [Effect::ProgramStatus(Update::Put(record))]
                if record.app.as_deref() == Some("a")),
            "{seen:?}"
        );

        let mut actor = crate::TerminalActor::new(20, 5).expect("new").actor;
        let mut rx = actor.subscribe_resume();
        actor.feed_for_test(report);
        assert_eq!(*rx.borrow_and_update(), ResumeToken::new("a", "one"));
        let json = serde_json::to_string(&actor.subscribe_program_status().borrow().clone());
        assert!(!json.expect("records").contains("one"), "not in a record");
        actor.feed_for_test(b"\x1b]133;A\x07");
        assert_eq!(*rx.borrow_and_update(), None);
    }
}
