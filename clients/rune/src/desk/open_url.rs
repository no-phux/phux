//! A pane asks to open a link on this machine
//! (`docs/ssh.md#opening-links`): held as a question, one at a time, and
//! declining is the default. Opening takes O or the button, and only after
//! [`QUIET`] with no key pressed, so what was being typed into the pane never
//! opens anything.

use std::time::{Duration, Instant};

use phux_config::open_url::WebLink;
use phux_protocol::ids::ResourceId;
use phux_protocol::open_url::OPEN_URL_SECS;
use rune::Tx;

use super::{Desk, Pending};

/// How long the question must be up with no key pressed before it can be
/// answered, except by Escape.
const QUIET: Duration = Duration::from_secs(1);
/// When the server takes the silence as a decline.
const DEADLINE: Duration = Duration::from_secs(OPEN_URL_SECS);
/// After a decline, how long further requests are declined unasked.
const COOLDOWN: Duration = Duration::from_secs(3);

/// The link a pane asks to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub terminal: ResourceId,
    id: u64,
    pub link: WebLink,
    /// The machine the pane is on, and its name in `--remote` when it is
    /// another one.
    pub machine: String,
    pub remote: Option<String>,
    since: Instant,
    /// Since when no key was pressed.
    quiet: Instant,
}

impl Desk {
    /// `terminal` asks to open `url` here. True when the window changed (a
    /// question went up).
    pub(super) fn open_url_asked(&mut self, terminal: ResourceId, id: u64, url: &str) -> bool {
        let cooling =
            (self.connection.resources.link_declined).is_some_and(|at| at.elapsed() < COOLDOWN);
        let link = WebLink::parse(url).ok();
        let Some(link) = link.filter(|_| self.pending.is_none() && !cooling) else {
            self.answer_open_url(&terminal, id, false);
            return false;
        };
        let now = Instant::now();
        self.pending = Some(Pending::OpenUrl(Box::new(Asked {
            terminal,
            id,
            link,
            machine: self.machines.label().to_owned(),
            remote: self.machines.current.clone(),
            since: now,
            quiet: now,
        })));
        true
    }

    fn answer_open_url(&self, terminal: &ResourceId, id: u64, opened: bool) {
        if let Some(client) = &self.connection.client {
            client.open_url_reply(terminal, id, opened);
        }
    }

    /// Take down the held link question, declining it. False when none is.
    pub(super) fn decline_open_url(&mut self) -> bool {
        if !matches!(self.pending, Some(Pending::OpenUrl(_))) {
            return false;
        }
        let Some(Pending::OpenUrl(asked)) = self.pending.take() else {
            return false;
        };
        self.connection.resources.link_declined = Some(Instant::now());
        self.answer_open_url(&asked.terminal, asked.id, false);
        true
    }

    /// Open the held link, if it has been up and quiet long enough.
    pub(super) fn accept_open_url(&mut self, tx: &mut Tx) {
        let armed =
            |asked: &Asked| asked.quiet.elapsed() >= QUIET && asked.since.elapsed() < DEADLINE;
        let Some(Pending::OpenUrl(asked)) = self
            .pending
            .take_if(|pending| matches!(pending, Pending::OpenUrl(asked) if armed(asked)))
        else {
            return;
        };
        let opened = tx.open_url(asked.link.as_str()).is_ok();
        self.answer_open_url(&asked.terminal, asked.id, opened);
    }

    /// The confirm bar's keys while a link is held: O opens it, Return and N
    /// decline, all three only after the quiet; Escape declines at once.
    /// `None` when no link is held.
    pub(super) fn open_url_key(&mut self, event: &rune::KeyEvent, tx: &mut Tx) -> Option<bool> {
        let Some(Pending::OpenUrl(asked)) = &mut self.pending else {
            return None;
        };
        let quiet = asked.quiet.elapsed() >= QUIET;
        let chord = event.mods.cmd || event.mods.ctrl || event.mods.alt;
        let is = |text: &str, key: &str| text.eq_ignore_ascii_case(key) && !chord && quiet;
        match &event.key {
            rune::Key::Escape => {
                self.decline_open_url();
            }
            rune::Key::Enter if quiet => {
                self.decline_open_url();
            }
            rune::Key::Text(text) if is(text, "n") => {
                self.decline_open_url();
            }
            rune::Key::Text(text) if is(text, "o") => self.accept_open_url(tx),
            _ => {
                asked.quiet = Instant::now();
                return Some(false);
            }
        }
        Some(true)
    }

    /// Take down a link nobody answered, and wake for when one will be.
    pub(super) fn tick_open_url(&mut self, tx: &mut Tx) -> bool {
        let Some(Pending::OpenUrl(asked)) = &self.pending else {
            return false;
        };
        let left = DEADLINE.saturating_sub(asked.since.elapsed());
        if left.is_zero() {
            return self.decline_open_url();
        }
        tx.wake_after(left);
        false
    }
}

#[cfg(test)]
impl Desk {
    /// Hold terminal 1's `url`, for a view test.
    pub fn hold_link_for_test(&mut self, url: &str) {
        self.open_url_asked(ResourceId::local(1), 1, url);
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::open_url::OpenUrlOp;
    use phux_protocol::wire::frame::{Command, FrameKind};
    use rune::{Env, Key, Runtime};

    use super::*;
    use crate::keys::plain;
    use crate::launch::Resolved;
    use crate::test_support::terminal::Replica;

    const LINK: &str = "https://id.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A8085%2F";

    /// A window on `mini`, its pane asking to open `url`.
    fn window(url: &str) -> Runtime<Desk> {
        let replica = Replica::new(20, 4, 1);
        let mut desk = Desk::with_preferences(
            &Resolved::offline("links"),
            None,
            rune::prefs::Prefs::memory(),
        );
        desk.connection.client = Some(replica.client);
        desk.machines.current = Some("mini".to_owned());
        desk.open_url_asked(id(), 3, url);
        crate::test_support::runtime(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0))
    }

    fn id() -> ResourceId {
        ResourceId::local(1)
    }

    /// The question has been up, untouched, for long enough to answer.
    fn settle(desk: &mut Desk) {
        if let Some(Pending::OpenUrl(asked)) = &mut desk.pending {
            asked.quiet -= QUIET;
        }
    }

    /// The answers sent since the last call.
    fn answers(desk: &Desk) -> Vec<bool> {
        let client = desk.connection.client.as_ref().expect("connected");
        client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
                FrameKind::Command {
                    command:
                        Command::OpenUrl {
                            terminal_id,
                            op: OpenUrlOp::Reply { id: 3, opened },
                        },
                    ..
                } if terminal_id == id() => Some(opened),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn opening_takes_o_after_a_quiet_second_and_nothing_typed_gets_there() {
        let mut rt = window(LINK);
        rt.update(|d, tx| {
            for text in ["o", "O", "n", "y", "g"] {
                let key = plain(Key::Text(text.into()));
                assert_eq!(d.open_url_key(&key, tx), Some(false), "{text} at once");
            }
            assert_eq!(d.open_url_key(&plain(Key::Enter), tx), Some(false));
            // The button waits out the quiet too.
            d.confirm_pending_tx(tx);
            assert!(d.pending.is_some(), "still asking");
            // A typed key starts the quiet again.
            settle(d);
            let y = plain(Key::Text("y".into()));
            assert_eq!(d.open_url_key(&y, tx), Some(false));
            let o = plain(Key::Text("o".into()));
            assert_eq!(d.open_url_key(&o, tx), Some(false));
            assert!(answers(d).is_empty());
            settle(d);
            assert_eq!(d.open_url_key(&o, tx), Some(true));
            assert_eq!(d.pending, None);
            assert_eq!(answers(d), [true]);
        });
        assert_eq!(rt.take_url_requests(), [LINK]);
    }

    #[test]
    fn return_escape_cancel_and_silence_decline() {
        let ways: [fn(&mut Desk, &mut Tx); 4] = [
            |d, tx| {
                settle(d);
                d.open_url_key(&plain(Key::Enter), tx);
            },
            |d, tx| {
                d.open_url_key(&plain(Key::Escape), tx);
            },
            |d, _| d.cancel_pending(),
            |d, tx| {
                assert!(!d.tick_open_url(tx), "not yet");
                if let Some(Pending::OpenUrl(asked)) = &mut d.pending {
                    asked.since -= DEADLINE;
                }
                settle(d);
                d.accept_open_url(tx);
                assert!(d.pending.is_some(), "too late to open");
                assert!(d.tick_open_url(tx));
            },
        ];
        for decline in ways {
            let mut rt = window(LINK);
            rt.update(|d, tx| {
                decline(d, tx);
                assert_eq!(d.pending, None);
                assert_eq!(answers(d), [false]);
                // Asking again straight away is declined unasked.
                assert!(!d.open_url_asked(id(), 3, LINK));
                assert_eq!(answers(d), [false]);
            });
            assert!(rt.take_url_requests().is_empty());
        }
    }

    #[test]
    fn a_bad_link_and_a_second_one_are_declined_unasked() {
        for bad in ["file:///etc/passwd", "https://a@evil.example/", "x"] {
            let rt = window(bad);
            assert_eq!(rt.state.pending, None);
            assert_eq!(answers(&rt.state), [false]);
        }
        let mut rt = window(LINK);
        rt.update(|d, _| {
            assert!(!d.open_url_asked(id(), 3, "https://other.example/"));
            assert_eq!(answers(d), [false]);
            assert!(
                matches!(&d.pending, Some(Pending::OpenUrl(asked)) if asked.link.as_str() == LINK)
            );
        });
    }
}
