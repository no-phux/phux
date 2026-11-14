//! A screenshot sent to a terminal: the system's region picker takes it,
//! then it goes the way a pasted image does, uploaded to the terminal's host
//! and its path typed at the prompt, for the agent in that pane to read.

use std::sync::{Arc, Mutex};

use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};
use crate::screenshot::{self, Shot};

/// A capture under way: who it is for, and where its worker leaves it.
pub struct Capture {
    terminal: ResourceId,
    /// The connection it was asked on; a move to another machine voids it.
    server: u64,
    done: Arc<Mutex<Option<Shot>>>,
}

impl Desk {
    /// Pick a region of the screen for the terminal with the keyboard now.
    pub(super) fn screenshot(&mut self) {
        if self.capture.is_some() {
            return self.say(NoticeKind::Info, "A screenshot is already being taken.");
        }
        let Some((terminal, view)) = self.keyboard() else {
            return self.say(NoticeKind::Error, "No terminal to send a screenshot to.");
        };
        if view.is_none() || self.is_document(terminal) || !self.pastes_allowed() {
            return;
        }
        if let Some(why) = screenshot::refusal() {
            return self.say(NoticeKind::Error, why);
        }
        let terminal = terminal.clone();
        let done: Arc<Mutex<Option<Shot>>> = Arc::default();
        let (slot, waker) = (Arc::clone(&done), self.waker.clone());
        std::thread::spawn(move || {
            let shot = screenshot::take();
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(shot);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
        self.capture = Some(Capture {
            terminal,
            server: self.link.server,
            done,
        });
    }

    /// The picker closing. True when the user should hear about it.
    pub(super) fn finish_screenshot(&mut self) -> bool {
        let Some(capture) = &self.capture else {
            return false;
        };
        let Some(shot) = capture.done.lock().ok().and_then(|mut slot| slot.take()) else {
            return false;
        };
        let Some(capture) = self.capture.take() else {
            return false;
        };
        match shot {
            Shot::Cancelled => return false,
            Shot::Failed(why) => {
                self.say(NoticeKind::Error, format!("Screenshot not taken: {why}"))
            }
            Shot::Taken(png) => self.send_screenshot(&capture, png),
        }
        true
    }

    /// To the terminal it was taken for, wherever the keyboard went since.
    fn send_screenshot(&mut self, capture: &Capture, png: Vec<u8>) {
        let refusal = if capture.server != self.link.server {
            Some("This window moved to another machine; the screenshot wasn't sent.")
        } else if self.client.is_none() {
            Some("Not connected; the screenshot wasn't sent.")
        } else if self.topology.is_some() && !self.lists(&capture.terminal) {
            Some("That terminal closed; the screenshot wasn't sent.")
        } else {
            None
        };
        match refusal {
            Some(why) => self.say(NoticeKind::Error, why),
            None => self.upload_image(&capture.terminal, png),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("not launched"))
    }

    fn connected(desk: &mut Desk) {
        desk.client = Some(Arc::new(phux_client_runtime::Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        )));
    }

    fn taken(desk: &mut Desk, terminal: u32, shot: Shot) {
        desk.capture = Some(Capture {
            terminal: ResourceId::local(terminal),
            server: desk.link.server,
            done: Arc::new(Mutex::new(Some(shot))),
        });
    }

    #[test]
    fn a_screenshot_uploads_for_the_terminal_it_was_taken_for() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 3, Shot::Taken(b"\x89PNG".to_vec()));
        assert!(desk.finish_screenshot());
        assert!(desk.capture.is_none());
        assert_eq!(desk.uploads.len(), 1);
        assert_eq!(desk.uploads[0].1, ResourceId::local(3));
    }

    #[test]
    fn a_cancel_says_nothing() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 1, Shot::Cancelled);
        assert!(!desk.finish_screenshot());
        assert!(desk.capture.is_none(), "the next one may start");
        assert!(desk.notice.is_none());
        assert!(desk.uploads.is_empty());
    }

    #[test]
    fn a_failure_is_heard() {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Failed("no picker".into()));
        assert!(desk.finish_screenshot());
        assert!(desk.notice.is_some());
    }

    #[test]
    fn a_screenshot_never_follows_the_window_to_another_server() {
        let mut desk = desk();
        connected(&mut desk);
        taken(&mut desk, 1, Shot::Taken(b"\x89PNG".to_vec()));
        desk.left_server();
        assert!(desk.finish_screenshot());
        assert!(desk.uploads.is_empty());
        assert!(desk.notice.is_some(), "the person hears why");
    }

    #[test]
    fn an_unfinished_capture_waits() {
        let mut desk = desk();
        desk.capture = Some(Capture {
            terminal: ResourceId::local(1),
            server: desk.link.server,
            done: Arc::default(),
        });
        assert!(!desk.finish_screenshot());
        assert!(desk.capture.is_some());
        desk.screenshot();
        assert!(desk.notice.is_some(), "one at a time, and said so");
    }

    #[test]
    fn no_terminal_no_picker() {
        let mut desk = desk();
        desk.screenshot();
        assert!(desk.capture.is_none());
        assert!(desk.notice.is_some());
    }
}
