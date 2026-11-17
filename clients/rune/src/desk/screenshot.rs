//! A screenshot sent to a terminal: the system's region picker takes it,
//! then it goes the way a pasted image does, uploaded to the terminal's host
//! and its path typed at the prompt, for the agent in that pane to read.

use std::sync::{Arc, Mutex};

use phux_client_runtime::control::PrivateUploadDestination;
use phux_protocol::ids::ResourceId;

use super::{Desk, NoticeKind};
use crate::screenshot::{self, Shot};

/// A capture under way: who it is for, and where its worker leaves it.
pub struct Capture {
    terminal: ResourceId,
    /// The connection it was asked on; a move to another machine voids it.
    server: u64,
    destination: PrivateUploadDestination,
    done: Arc<Mutex<Option<Shot>>>,
    picker: screenshot::Picker,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.picker.cancel();
        if let Some(worker) = self.worker.take() {
            // The worker owns private files. Reap it before window/app exit,
            // after cancelling only its recorded picker child.
            let _ = worker.join();
        }
    }
}

impl Desk {
    /// Closing a keep-running window hides it without dropping its Desk.
    pub(crate) fn cancel_screenshot(&mut self) {
        self.connection.capture = None;
    }

    /// Pick a region of the screen for the terminal with the keyboard now.
    pub(super) fn screenshot(&mut self) {
        if self.connection.capture.is_some() {
            return self.say(NoticeKind::Info, "A screenshot is already being taken.");
        }
        let Some((terminal, view)) = self.keyboard() else {
            return self.say(NoticeKind::Error, "No terminal to send a screenshot to.");
        };
        if view.is_none() || self.is_document(terminal) || !self.pastes_allowed() {
            return;
        }
        let destination = self.connection.client.as_ref().and_then(|client| {
            client.with_control(|control| control.private_upload_destination(terminal))
        });
        let Some(destination) = destination else {
            return self.say(NoticeKind::Error,
                "Screenshot not taken: this terminal's live execution cannot be verified. Reconnect to its host directly; older servers and satellite panes aren't supported.");
        };
        if let Some(why) = screenshot::refusal() {
            return self.say(NoticeKind::Error, why);
        }
        let terminal = terminal.clone();
        let done: Arc<Mutex<Option<Shot>>> = Arc::default();
        let (slot, waker) = (Arc::clone(&done), self.waker.clone());
        let picker = screenshot::Picker::default();
        let worker_picker = picker.clone();
        let worker = std::thread::spawn(move || {
            let shot = screenshot::take(&worker_picker);
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(shot);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
        self.connection.capture = Some(Capture {
            terminal,
            server: self.link.server,
            destination,
            done,
            picker,
            worker: Some(worker),
        });
    }

    /// The picker closing. True when the user should hear about it.
    pub(super) fn finish_screenshot(&mut self) -> bool {
        let Some(capture) = &self.connection.capture else {
            return false;
        };
        let Some(shot) = capture.done.lock().ok().and_then(|mut slot| slot.take()) else {
            return false;
        };
        let Some(capture) = self.connection.capture.take() else {
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
        } else if !self.pastes_allowed() {
            Some("Another field or question has the keyboard; the screenshot wasn't sent.")
        } else if self.connection.client.is_none() {
            Some("Not connected; the screenshot wasn't sent.")
        } else if self.connection.topology.is_some() && !self.lists(&capture.terminal) {
            Some("That terminal closed; the screenshot wasn't sent.")
        } else {
            None
        };
        match refusal {
            Some(why) => self.say(NoticeKind::Error, why),
            None => {
                let Some(client) = &self.connection.client else {
                    return;
                };
                let transfer = client
                    .with_control(|control| control.put_private_image(&capture.destination, png));
                self.connection.uploads.push((
                    transfer,
                    capture.terminal.clone(),
                    Some(capture.destination.clone()),
                ));
                self.say(NoticeKind::Info, "Uploading image…");
            }
        }
    }

    /// Check and paste while holding the runtime's one control lock. A
    /// completed upload is not permission to deliver into a replacement run.
    pub(super) fn deliver_private_image(
        &mut self,
        destination: &PrivateUploadDestination,
        line: &str,
    ) -> bool {
        if !self.pastes_allowed() {
            return false;
        }
        let Some(client) = &self.connection.client else {
            return false;
        };
        let sent =
            client.with_control(|control| control.paste_private_image_path(destination, line));
        if sent {
            self.reclaim_fits();
        }
        sent
    }
}

#[cfg(test)]
mod tests;
