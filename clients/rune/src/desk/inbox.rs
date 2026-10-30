//! The inbox: answering what agents ask and deciding what the server holds,
//! from anywhere, and keeping notifications in step with it.

use std::collections::HashMap;

use phux_client_runtime::Client;
use phux_client_runtime::approvals::{Approval, Decision, DecisionOutcome};
use phux_client_runtime::control::{DeliveryOutcome, Event};
use phux_protocol::ids::{ApprovalId, ResourceId};
use rune::{Key, KeyEvent, TextEdit, Tx};

use super::Desk;
use crate::alerts::{Alerts, Target};
use crate::inbox::{self, Item};

#[derive(Default)]
pub struct Inbox {
    pub open: bool,
    pub reply: TextEdit,
    pub picked: usize,
    pub approvals: Vec<Approval>,
    /// Answers on their way, to report how each ended.
    pub answers: HashMap<u64, ResourceId>,
    pub alerts: Alerts,
}

impl Desk {
    pub fn inbox_items(&self) -> Vec<Item> {
        inbox::items(
            self.topology.as_ref(),
            &self.activities,
            &self.inbox.approvals,
        )
    }

    fn picked_item(&self) -> Option<Item> {
        let items = self.inbox_items();
        let last = items.len().checked_sub(1)?;
        items.get(self.inbox.picked.min(last)).cloned()
    }

    pub fn toggle_inbox(&mut self) {
        if self.inbox.open {
            self.close_inbox();
            return;
        }
        self.inbox.open = true;
        self.inbox.picked = 0;
        self.inbox.reply.clear();
        self.inbox.reply.focused = true;
    }

    pub fn close_inbox(&mut self) {
        self.inbox.open = false;
        self.inbox.reply.clear();
        self.inbox.reply.focused = false;
    }

    pub fn pick_item(&mut self, picked: usize, tx: &mut Tx) {
        if picked != self.inbox.picked {
            self.inbox.reply.clear();
        }
        self.inbox.picked = picked;
        self.inbox.reply.focused = true;
        tx.scroll_into_view(format!("inbox-item-{picked}"));
    }

    /// Keys while the inbox is open. The reply field gets typing; everything
    /// here applies whether or not it holds the focus.
    pub fn inbox_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        let count = self.inbox_items().len();
        match event.key {
            Key::Escape => {
                self.close_inbox();
                tx.blur();
            }
            Key::Down | Key::Tab if count > 0 => {
                self.pick_item((self.inbox.picked + 1) % count, tx)
            }
            Key::Up if count > 0 => self.pick_item((self.inbox.picked + count - 1) % count, tx),
            Key::Enter if event.mods.cmd => self.decide_picked(Decision::Approve),
            Key::Backspace if event.mods.cmd => self.decide_picked(Decision::Deny),
            Key::Enter => self.submit_picked(tx),
            Key::Command(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                self.answer_suggestion(index);
            }
            _ => return false,
        }
        true
    }

    /// Return: send the draft, or go to the terminal when there is nothing
    /// to send. A held action is never decided by plain Return.
    pub fn submit_picked(&mut self, tx: &mut Tx) {
        let Some(Item::Terminal(waiting)) = self.picked_item() else {
            return;
        };
        let draft = self.inbox.reply.text.trim().to_owned();
        match waiting.ask {
            Some(ask) if !draft.is_empty() => self.answer(&waiting.terminal, &ask.id, &draft),
            _ => {
                self.close_inbox();
                tx.blur();
                self.show(waiting.terminal, &waiting.session);
            }
        }
    }

    fn answer_suggestion(&mut self, index: usize) {
        let Some(Item::Terminal(waiting)) = self.picked_item() else {
            return;
        };
        let Some(ask) = waiting.ask else { return };
        if let Some(text) = ask.suggestions.get(index) {
            self.answer(&waiting.terminal, &ask.id, text);
        }
    }

    /// Type `text` as the answer to `question`. The runtime refuses a stale
    /// question, so a reply can't land on whatever the agent moved on to.
    pub fn answer(&mut self, terminal: &ResourceId, question: &str, text: &str) {
        let Some(client) = &self.client else { return };
        if self.inbox.answers.values().any(|t| t == terminal) {
            return;
        }
        let delivery = client.apply_answer(terminal, question, text);
        self.inbox.answers.insert(delivery, terminal.clone());
    }

    pub fn answering(&self, terminal: &ResourceId) -> bool {
        self.inbox.answers.values().any(|t| t == terminal)
    }

    fn decide_picked(&mut self, decision: Decision) {
        if let Some(Item::Approval(approval)) = self.picked_item() {
            self.decide(&approval.id, decision);
        }
    }

    /// Send one decision. Never retried: the event reports how it ended.
    pub fn decide(&mut self, id: &str, decision: Decision) {
        let Some(client) = &self.client else { return };
        let Some(id) = ApprovalId::parse(id) else {
            return;
        };
        if !client.decide_approval(id, decision) {
            self.notice = Some("That decision is already on its way.".to_owned());
        }
    }

    /// Answers and decisions coming back, and the held set changing.
    pub(super) fn inbox_event(&mut self, client: &Client, event: &Event) -> bool {
        match event {
            Event::ApprovalsChanged => {
                self.inbox.approvals = client.approvals();
                true
            }
            Event::ApprovalDecision { outcome, .. } => {
                self.notice = decision_notice(outcome);
                true
            }
            Event::InputDelivery {
                delivery_id,
                outcome,
                message,
                ..
            } if self.inbox.answers.remove(delivery_id).is_some() => {
                self.notice = match outcome {
                    DeliveryOutcome::Delivered => {
                        self.inbox.reply.clear();
                        None
                    }
                    DeliveryOutcome::Refused => Some(format!("Answer not sent: {message}")),
                    DeliveryOutcome::Unknown => {
                        Some("The answer may not have landed; read the pane first.".to_owned())
                    }
                };
                true
            }
            _ => false,
        }
    }

    /// Post, take back, and count; then follow any notification clicked.
    pub(super) fn sync_alerts(&mut self) {
        let items = self.inbox_items();
        let on_screen = self.on_screen();
        let (focused, open) = (self.focused, self.inbox.open);
        let enabled = self.settings.notifications;
        self.inbox.alerts.sync(
            &items,
            |target| match target {
                Target::Inbox => focused && open,
                Target::Terminal { id, .. } => focused && on_screen.contains(id),
            },
            enabled,
        );
        for target in self.inbox.alerts.take_clicks() {
            rune_mac::notify::bring_to_front();
            match target {
                Target::Inbox if !self.inbox.open => self.toggle_inbox(),
                Target::Inbox => {}
                Target::Terminal { id, session } => self.show(id, &session),
            }
        }
    }
}

fn decision_notice(outcome: &DecisionOutcome) -> Option<String> {
    match outcome {
        DecisionOutcome::Decided => None,
        DecisionOutcome::Stale => Some("That action was already settled.".to_owned()),
        DecisionOutcome::Refused { message, .. } => Some(format!("Refused: {message}")),
        _ => Some("The server never confirmed that decision; check before retrying.".to_owned()),
    }
}
