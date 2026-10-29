//! Per-subscription delivery: admission, live offers, cursor replay, and
//! owed gaps.

use std::collections::HashMap;
use std::sync::Arc;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::scope::EffectiveScopeSet;
use phux_protocol::wire::frame::{AgentEvent, ControlAction, FrameKind};
use tokio::sync::{Notify, mpsc};

use super::{EventScope, EventSubscription, PendingReplay, journal_gap_frame};
use crate::mailbox::Outbound;
use crate::state::journal::{Journal, JournalEntry, NextEntry};

impl EventSubscription {
    pub(in crate::state) fn new(
        tx: mpsc::Sender<Outbound>,
        epoch: u64,
        observe: Option<EffectiveScopeSet>,
    ) -> Self {
        Self {
            tx,
            scopes: HashMap::new(),
            journal_aware: false,
            gap: None,
            replay: None,
            delivered: 0,
            wake: Arc::new(Notify::new()),
            pump_claimed: false,
            epoch,
            observe,
        }
    }

    /// Wake the pump so it notices the subscription is gone.
    pub(in crate::state) fn retire(&self) {
        self.wake.notify_one();
    }

    /// Whether any scope admits `entry`. `ROLE_CHANGED` goes only to
    /// journal-aware subscriptions (older decoders fail on it).
    pub(super) fn admits(&self, entry: &JournalEntry) -> bool {
        if !self.journal_aware && is_role_changed(&entry.event) {
            return false;
        }
        if let Some(effective) = &self.observe
            && !crate::policy::filter::observes_event(
                effective,
                entry.terminal.as_ref(),
                entry.parent.as_ref(),
                &entry.subject,
            )
        {
            return false;
        }
        self.scopes
            .iter()
            .any(|(scope, filter)| scope.covers(entry) && filter.admits(&entry.event))
    }

    /// The newest `seq` sent or owed (a satellite cursor is owed a gap).
    pub(super) fn reach(&self) -> u64 {
        let last = |gap: Option<(u64, u64)>| gap.map_or(0, |(_, last)| last);
        self.delivered
            .max(last(self.gap))
            .max(last(self.replay.and_then(|replay| replay.leading_gap)))
    }

    /// The newest evicted `seq` while a replay is pending below it (a gap
    /// will be sent); else `0`.
    pub(super) fn owed_eviction(&self, journal: &Journal) -> u64 {
        let evicted = journal.evicted_through();
        match self.replay {
            Some(replay) if replay.cursor.max(self.delivered) < evicted => evicted,
            _ => 0,
        }
    }

    /// The frame this subscription receives for `entry`.
    fn frame_for(&self, entry: &JournalEntry) -> FrameKind {
        entry.frame_with(render_for(&entry.event, self.journal_aware))
    }

    /// Deliver `entry` if in scope and newer than anything given, reporting
    /// earlier loss first.
    pub(super) fn offer(&mut self, entry: &JournalEntry) {
        if entry.seq() <= self.delivered {
            return;
        }
        let admitted = self.admits(entry);
        if self.replay.is_some() {
            // The replay reaches retained events; relayed ones it cannot.
            if admitted && entry.is_relayed() {
                self.owe_gap(entry.seq(), entry.seq());
            }
            return;
        }
        if !admitted {
            let _ = self.flush_gap();
            return;
        }
        if !self.flush_gap() || !self.try_send(self.frame_for(entry)) {
            self.owe_gap(entry.seq(), entry.seq());
            return;
        }
        self.delivered = entry.seq();
    }

    /// Queue what is owed while the mailbox has room; wake the pump for the
    /// rest.
    pub(super) fn fill(&mut self, journal: &Journal) {
        let tx = self.tx.clone();
        while let Ok(permit) = tx.try_reserve() {
            let Some(frame) = self.next_owed(journal) else {
                return;
            };
            permit.send(Outbound::Frame(frame));
        }
        if self.is_owed_work() {
            self.wake.notify_one();
        }
    }

    pub(in crate::state) const fn is_owed_work(&self) -> bool {
        self.gap.is_some() || self.replay.is_some()
    }

    /// The next owed frame: replay first, then a live-event gap.
    pub(super) fn next_owed(&mut self, journal: &Journal) -> Option<FrameKind> {
        if self.replay.is_some() {
            return self.next_replay_frame(journal);
        }
        self.take_gap_frame()
    }

    /// The next replay frame, skipping anything already given, with any gap
    /// owed meanwhile sent before later `seq`s.
    fn next_replay_frame(&mut self, journal: &Journal) -> Option<FrameKind> {
        let replay = self.replay?;
        if let Some((first, last)) = replay.leading_gap {
            self.replay = Some(PendingReplay {
                cursor: replay.cursor.max(last),
                leading_gap: None,
            });
            return Some(self.report_gap(first, last));
        }
        let next = journal.next_after(replay.cursor.max(self.delivered), |entry| {
            self.admits(entry)
        });
        if self.owed_gap_precedes(&next) {
            return self.take_gap_frame();
        }
        match next {
            NextEntry::Gap(first, last) => {
                self.advance_replay(last);
                Some(self.report_gap(first, last))
            }
            NextEntry::Entry(entry) => {
                self.advance_replay(entry.seq());
                self.delivered = self.delivered.max(entry.seq());
                Some(self.frame_for(&entry))
            }
            NextEntry::CaughtUp => {
                self.replay = None;
                self.take_gap_frame()
            }
        }
    }

    /// Whether the owed live gap precedes the replay's next frame.
    fn owed_gap_precedes(&self, next: &NextEntry) -> bool {
        let Some((owed_first, _)) = self.gap else {
            return false;
        };
        match next {
            NextEntry::Gap(first, _) => *first > owed_first,
            NextEntry::Entry(entry) => entry.seq() > owed_first,
            NextEntry::CaughtUp => false,
        }
    }

    const fn advance_replay(&mut self, cursor: u64) {
        if let Some(replay) = self.replay.as_mut() {
            replay.cursor = cursor;
        }
    }

    /// The `journal_gap` notice for `first..=last`, now reported.
    fn report_gap(&mut self, first: u64, last: u64) -> FrameKind {
        self.delivered = self.delivered.max(last);
        journal_gap_frame(first, last)
    }

    fn take_gap_frame(&mut self) -> Option<FrameKind> {
        let (first, last) = self.gap.take()?;
        Some(self.report_gap(first, last))
    }

    /// Send the pending gap, if any; `false` while still owed.
    fn flush_gap(&mut self) -> bool {
        let Some((first, last)) = self.gap else {
            return true;
        };
        let sent = self.try_send(journal_gap_frame(first, last));
        if sent {
            self.gap = None;
            self.delivered = self.delivered.max(last);
        }
        sent
    }

    pub(super) fn try_send(&self, frame: FrameKind) -> bool {
        self.tx.try_send(Outbound::Frame(frame)).is_ok()
    }

    /// Whether this subscription should see a live `input_activity` for
    /// `terminal`. The type filter is bypassed (presence, not grid
    /// activity). A scoped grant still has to `OBSERVE` the Terminal.
    pub(super) fn admits_input_activity(
        &self,
        terminal: &WireResourceId,
        subject: &crate::policy::filter::EventSubject,
    ) -> bool {
        if let Some(effective) = &self.observe
            && !crate::policy::filter::observes_event(effective, Some(terminal), None, subject)
        {
            return false;
        }
        self.scopes
            .keys()
            .any(|scope| scope_covers_terminal(scope, terminal))
    }

    /// Owe a `journal_gap` for `first..=last` and wake the pump.
    pub(super) fn owe_gap(&mut self, first: u64, last: u64) {
        self.gap = Some(match self.gap {
            Some((prior_first, prior_last)) => (prior_first.min(first), prior_last.max(last)),
            None => (first, last),
        });
        self.wake.notify_one();
    }

    /// Start a replay after `after_seq`, skipping what was already given.
    pub(super) fn begin_replay(&mut self, after_seq: u64, head: u64) {
        let start = replay_start(after_seq, head, self.delivered);
        let Some((cursor, leading_gap)) = start else {
            return;
        };
        // An already-pending replay keeps its (later) position.
        self.replay = Some(self.replay.map_or(
            PendingReplay {
                cursor,
                leading_gap,
            },
            |pending| PendingReplay {
                leading_gap: merge_gap(pending.leading_gap, leading_gap),
                ..pending
            },
        ));
    }
}

/// Where a replay after `after_seq` starts, given `head` and `delivered`:
/// its cursor and leading gap, or `None` when nothing is owed.
fn replay_start(after_seq: u64, head: u64, delivered: u64) -> Option<(u64, Option<(u64, u64)>)> {
    if after_seq == u64::MAX || (after_seq == head && delivered <= head) {
        return None;
    }
    if after_seq > head {
        // Void: never issued by this incarnation (L1 §7.3).
        return Some((head.max(delivered), Some((1, head))));
    }
    if after_seq < delivered {
        // Already given up to `delivered` via another scope.
        return Some((delivered, Some((after_seq + 1, delivered))));
    }
    Some((after_seq, None))
}

fn merge_gap(a: Option<(u64, u64)>, b: Option<(u64, u64)>) -> Option<(u64, u64)> {
    match (a, b) {
        (Some((a_first, a_last)), Some((b_first, b_last))) => {
            Some((a_first.min(b_first), a_last.max(b_last)))
        }
        (one, other) => one.or(other),
    }
}

/// Whether `event` is a `terminal_control { ROLE_CHANGED }` (ADR-0127).
const fn is_role_changed(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TerminalControl {
            action: ControlAction::RoleChanged,
            ..
        }
    )
}

/// How `event` reads to a subscriber: `EXPIRED` becomes actor-less
/// `RELEASED` for one that never sent a cursor (L1 §7.1).
fn render_for(event: &AgentEvent, journal_aware: bool) -> AgentEvent {
    match event {
        AgentEvent::TerminalControl {
            lifecycle,
            exit_status,
            input_holder,
            action: ControlAction::Expired,
            ..
        } if !journal_aware => AgentEvent::TerminalControl {
            lifecycle: *lifecycle,
            exit_status: *exit_status,
            input_holder: *input_holder,
            action: ControlAction::Released,
            actor: None,
        },
        other => other.clone(),
    }
}

/// Whether `scope` delivers a live event about `terminal`. A server-wide
/// scope sees local Terminals only; a relayed satellite event is never
/// server-wide.
fn scope_covers_terminal(scope: &EventScope, terminal: &WireResourceId) -> bool {
    match scope {
        EventScope::Server => terminal.is_local(),
        EventScope::Terminal(id) => id == terminal,
    }
}
