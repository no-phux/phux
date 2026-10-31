//! The inbox: answering what agents ask and deciding what the server holds,
//! from anywhere, jumping to what needs you, and keeping notifications in
//! step with it.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use phux_client_runtime::Client;
use phux_client_runtime::approvals::{Approval, Decision, DecisionOutcome};
use phux_client_runtime::control::{DeliveryOutcome, Event, Status};
use phux_protocol::ids::{ApprovalId, ResourceId};
use rune::{Key, KeyEvent, Mods, TextEdit, Tx};

use super::{Desk, NoticeKind};
use crate::alerts::{Alerts, Target};
use crate::inbox::{self, Item, Key as ItemKey};

/// Everything the alerts and the Dock are derived from. While it holds still
/// there is nothing to post, take back or count, however busy the terminals.
#[derive(Debug, PartialEq)]
pub struct AlertInputs {
    marks: u64,
    /// Panes' identities and labels, and the held actions.
    listed: u64,
    on_screen: Vec<ResourceId>,
    focused: bool,
    open: bool,
    enabled: bool,
}

#[derive(Default)]
pub struct Inbox {
    pub open: bool,
    pub reply: TextEdit,
    /// The row the keyboard is on, by identity: rows move as work arrives,
    /// and a draft or a decision must never slide onto another one.
    pub picked: Option<ItemKey>,
    pub approvals: Vec<Approval>,
    /// Answers on their way, to report how each ended.
    pub answers: HashMap<u64, ResourceId>,
    pub alerts: Alerts,
    /// What the alerts last synced from.
    pub synced: Option<AlertInputs>,
    /// A new connection hasn't yet told marks from its old one's leftovers.
    pub relist: bool,
}

impl Desk {
    pub fn inbox_items(&self) -> Vec<Item> {
        inbox::items(
            self.topology.as_ref(),
            &self.activities,
            &self.inbox.approvals,
        )
    }

    /// The picked row's place in `items`.
    pub fn picked_index(&self, items: &[Item]) -> Option<usize> {
        let picked = self.inbox.picked.as_ref()?;
        items.iter().position(|item| &item.key() == picked)
    }

    fn picked_item(&self) -> Option<Item> {
        let items = self.inbox_items();
        items.get(self.picked_index(&items)?).cloned()
    }

    /// A picked row that left takes its draft with it; the top row is next.
    fn keep_pick(&mut self, items: &[Item]) {
        if self.inbox.picked.is_some() && self.picked_index(items).is_none() {
            self.inbox.picked = None;
            self.inbox.reply.clear();
        }
        if self.inbox.picked.is_none() {
            self.inbox.picked = items.first().map(Item::key);
        }
    }

    pub fn toggle_inbox(&mut self) {
        if self.inbox.open {
            self.close_inbox();
            return;
        }
        self.settings_open = false;
        self.inbox.open = true;
        self.inbox.picked = None;
        self.keep_pick(&self.inbox_items());
        self.inbox.reply.clear();
        self.inbox.reply.focused = true;
    }

    pub fn close_inbox(&mut self) {
        self.inbox.open = false;
        self.inbox.reply.clear();
        self.inbox.reply.focused = false;
    }

    pub fn pick_item(&mut self, index: usize, tx: &mut Tx) {
        let items = self.inbox_items();
        let Some(key) = items.get(index).map(Item::key) else {
            return;
        };
        if self.inbox.picked.as_ref() != Some(&key) {
            self.inbox.reply.clear();
        }
        self.inbox.picked = Some(key);
        self.inbox.reply.focused = true;
        tx.scroll_into_view(format!("inbox-item-{index}"));
    }

    /// Keys while the inbox is open. The reply field gets typing; everything
    /// here applies whether or not it holds the focus.
    pub fn inbox_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        let items = self.inbox_items();
        let (count, at) = (items.len(), self.picked_index(&items).unwrap_or(0));
        let chord = decision_chord(&event.mods, cfg!(target_os = "macos")) && !tx.key_repeat();
        match event.key {
            Key::Escape => {
                self.close_inbox();
                tx.blur();
            }
            Key::Down | Key::Tab if count > 0 => self.pick_item((at + 1) % count, tx),
            Key::Up if count > 0 => self.pick_item((at + count - 1) % count, tx),
            Key::Enter if chord => self.decide_picked(Decision::Approve),
            Key::Backspace if chord => self.decide_picked(Decision::Deny),
            Key::Enter if event.mods.cmd || event.mods.shift => {}
            Key::Enter => self.submit_picked(tx),
            Key::Command(c) => {
                let digit = tx
                    .physical_key()
                    .and_then(crate::keys::us_char)
                    .unwrap_or(c);
                match digit {
                    '1'..='9' => self.answer_suggestion(digit as usize - '1' as usize),
                    _ => return false,
                }
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
            Some(ask) if ask.typed() && !draft.is_empty() => {
                self.answer(&waiting.terminal, &ask.id, &draft)
            }
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
        let Some(ask) = waiting.ask.filter(|ask| ask.typed()) else {
            return;
        };
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

    /// Show what needs a person most, or the next after the shown one.
    pub fn jump_to_attention(&mut self) {
        let Some(topology) = &self.topology else {
            return;
        };
        let shown = self.shown.as_ref().map(|s| &s.terminal);
        let next = inbox::next_jump(&topology.panes, &self.activities, shown)
            .map(|p| (p.terminal_id.clone(), p.session_name.clone()));
        if let Some((terminal, session)) = next {
            self.show(terminal, &session);
        }
    }

    /// Send one decision. Never retried: the event reports how it ended.
    pub fn decide(&mut self, id: &str, decision: Decision) {
        let Some(client) = &self.client else { return };
        let Some(id) = ApprovalId::parse(id) else {
            return;
        };
        if !client.decide_approval(id, decision) {
            self.say(NoticeKind::Error, "That decision is already on its way.");
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
                match decision_notice(outcome) {
                    Some(text) => self.say(NoticeKind::Error, text),
                    None => self.notice = None,
                }
                true
            }
            Event::InputDelivery {
                delivery_id,
                outcome,
                message,
                ..
            } if self.inbox.answers.remove(delivery_id).is_some() => {
                match outcome {
                    DeliveryOutcome::Delivered => {
                        self.inbox.reply.clear();
                        self.notice = None;
                    }
                    DeliveryOutcome::Refused => {
                        self.say(NoticeKind::Error, format!("Answer not sent: {message}"));
                    }
                    DeliveryOutcome::Unknown => self.say(
                        NoticeKind::Error,
                        "The answer may not have landed; read the pane first.",
                    ),
                }
                true
            }
            _ => false,
        }
    }

    fn alert_inputs(&self) -> AlertInputs {
        let mut listed = DefaultHasher::new();
        for pane in self.topology.iter().flat_map(|t| t.panes.iter()) {
            (&pane.terminal_id, &pane.session_name, &pane.title).hash(&mut listed);
            (&pane.name, &pane.cwd).hash(&mut listed);
        }
        for approval in &self.inbox.approvals {
            approval.id.hash(&mut listed);
        }
        AlertInputs {
            marks: self.activities.revision(),
            listed: listed.finish(),
            on_screen: self.on_screen(),
            focused: self.focused,
            open: self.inbox.open,
            enabled: self.settings.notifications,
        }
    }

    /// Once a reconnect has attached, its list and replica are the truth.
    fn relist(&mut self) -> bool {
        let ready = self.inbox.relist && self.status == Status::Attached;
        let (Some(client), Some(topology), true) = (&self.client, &self.topology, ready) else {
            return false;
        };
        let listed: Vec<ResourceId> = topology
            .panes
            .iter()
            .map(|pane| pane.terminal_id.clone())
            .collect();
        self.inbox.relist = false;
        self.activities
            .reconnected(&listed, |id| client.command_running(id))
    }

    /// Post, take back, and count when what they read from moved; then
    /// follow any notification clicked.
    pub(super) fn sync_alerts(&mut self) -> bool {
        let relisted = self.relist();
        let inputs = self.alert_inputs();
        if self.inbox.synced.as_ref() == Some(&inputs) && !self.inbox.alerts.clicked() {
            return relisted;
        }
        let items = self.inbox_items();
        self.keep_pick(&items);
        let (focused, open) = (inputs.focused, inputs.open);
        let (on_screen, enabled) = (&inputs.on_screen, inputs.enabled);
        self.inbox.alerts.sync(
            &items,
            |target| match target {
                Target::Inbox => focused && open,
                Target::Terminal { id, .. } => focused && on_screen.contains(id),
            },
            enabled,
        );
        self.inbox.synced = Some(inputs);
        let clicks = self.inbox.alerts.take_clicks();
        let clicked = !clicks.is_empty();
        for target in clicks {
            rune_mac::notify::bring_to_front();
            match target {
                Target::Inbox if !self.inbox.open => self.toggle_inbox(),
                Target::Inbox => {}
                Target::Terminal { id, session } => self.show(id, &session),
            }
        }
        relisted | clicked
    }
}

/// The exact chord a decision takes: ⌘ on macOS, Ctrl+Shift elsewhere, where
/// Ctrl alone belongs to the reply field. Plain Return never qualifies.
const fn decision_chord(mods: &Mods, macos: bool) -> bool {
    mods.cmd && mods.shift != macos && !mods.alt
}

fn decision_notice(outcome: &DecisionOutcome) -> Option<String> {
    match outcome {
        DecisionOutcome::Decided => None,
        DecisionOutcome::Stale => Some("That action was already settled.".to_owned()),
        DecisionOutcome::Refused { message, .. } => Some(format!("Refused: {message}")),
        _ => Some("The server never confirmed that decision; check before retrying.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decision_takes_the_platforms_exact_chord() {
        let cmd = Mods {
            cmd: true,
            ..Mods::default()
        };
        let cmd_shift = Mods { shift: true, ..cmd };
        let alt = Mods {
            alt: true,
            ..cmd_shift
        };
        assert!(decision_chord(&cmd, true));
        assert!(!decision_chord(&cmd_shift, true));
        assert!(decision_chord(&cmd_shift, false));
        assert!(!decision_chord(&cmd, false));
        for macos in [true, false] {
            assert!(!decision_chord(&Mods::default(), macos));
            assert!(!decision_chord(&alt, macos));
        }
    }
}
