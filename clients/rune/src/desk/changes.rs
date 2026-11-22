//! Window effects for the feature-owned read-only Changes panel.

use phux_client_runtime::Client;
use phux_client_runtime::control::Status;
use rune::{Key, KeyEvent, Tx};

use super::{Desk, NoticeKind};

impl Desk {
    pub(super) fn open_changes(&mut self) {
        let shown = self.connection.shown.as_ref().map(|s| s.resource.clone());
        let Some(terminal) = shown.filter(|id| !self.is_document(id)) else {
            return self.say(NoticeKind::Error, "Focus a terminal to see its changes.");
        };
        self.close_inbox();
        self.close_settings();
        self.connection.changes.open(terminal);
        self.refresh_changes();
    }

    pub fn close_changes(&mut self) {
        self.connection.changes.close();
    }

    pub fn refresh_changes(&mut self) {
        self.connection.changes.refresh(
            self.connection.client.as_deref(),
            self.connection.status == Status::Attached,
        );
    }

    pub(super) fn take_changes(&mut self, client: &Client) -> bool {
        self.connection
            .changes
            .take_answers(client, self.connection.status == Status::Attached)
    }

    pub fn pick_change(&mut self, index: usize, tx: &mut Tx) {
        self.connection.changes.pick(
            index,
            self.connection.client.as_deref(),
            self.connection.status == Status::Attached,
        );
        tx.scroll_into_view(format!("changes-row-{index}"));
    }

    pub(super) fn changes_key(&mut self, event: &KeyEvent, tx: &mut Tx) {
        let (count, at) = (
            self.connection.changes.rows().len(),
            self.connection.changes.picked,
        );
        match &event.key {
            Key::Escape => self.close_changes(),
            Key::Down | Key::Tab if count > 0 => self.pick_change((at + 1) % count, tx),
            Key::Up if count > 0 => self.pick_change((at + count - 1) % count, tx),
            Key::Text(key) if key == "r" => self.refresh_changes(),
            _ => {}
        }
    }
}
