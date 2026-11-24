//! The view size lock: while this window holds it, other clients viewing
//! the session no longer resize its panes (`phux.view.size-lock/v1`).
//!
//! The server ends the lock with the attachment, so a held lock is claimed
//! again on every `ATTACHED`.

use phux_protocol::wire::frame::{FrameKind, Scope, VIEW_SIZE_LOCK_KEY};

use super::{Desk, NoticeKind};
use crate::palette::{Item, Target};

/// The palette row that flips the lock.
pub fn item(locked: bool) -> Item {
    let (title, detail) = if locked {
        ("Unlock view size", "let other clients resize these panes")
    } else {
        (
            "Lock view size",
            "keep these pane sizes whoever else watches",
        )
    };
    Item {
        title: title.to_owned(),
        detail: detail.to_owned(),
        kind: "size-lock",
        target: Target::SizeLock(!locked),
    }
}

impl Desk {
    /// Claim or release the view size lock.
    pub(super) fn set_size_lock(&mut self, locked: bool) {
        if self.connection.client.is_none() {
            self.say(NoticeKind::Error, "Not connected to a server");
            return;
        }
        self.connection.size_locked = locked;
        self.claim_size_lock(locked);
        let words = if locked {
            "View size locked"
        } else {
            "View size unlocked"
        };
        self.say(NoticeKind::Info, words);
    }

    /// Write the lock for this window's attachment.
    pub(super) fn claim_size_lock(&self, locked: bool) {
        if let Some(client) = &self.connection.client {
            client.queue_frame(&FrameKind::SetMetadata {
                request_id: client.next_request_id(),
                scope: Scope::Global,
                key: VIEW_SIZE_LOCK_KEY.to_owned(),
                value: if locked {
                    b"true".to_vec()
                } else {
                    b"false".to_vec()
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_offers_the_other_state() {
        assert!(matches!(item(false).target, Target::SizeLock(true)));
        assert_eq!(item(true).title, "Unlock view size");
    }
}
