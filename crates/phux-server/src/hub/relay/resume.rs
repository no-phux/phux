//! Asking the satellite for a fresh bootstrap on a consumer's behalf: after a
//! redial, after a slow consumer's delivery overflowed, and when none came.

use std::time::{Duration, Instant};

use super::{
    BOOTSTRAP_PATIENCE, BootstrapLimits, BootstrapProfile, Command, FrameKind, MAX_BOOTSTRAP_ASKS,
    OVERFLOW_SPACING, ProxySubscriber, ProxySubscription, RelaySession, ResourceId, SnapshotGate,
    Unsubscribe, oneshot, warn,
};
use crate::hub::operation_fence::Incarnation;

/// A content subscriber's attach, kept so the hub can repeat it.
#[derive(Debug)]
pub(super) struct ContentAttach {
    pub(super) resource_kind: Option<phux_protocol::ids::ResourceKind>,
    pub(super) bootstrap_profile: Option<BootstrapProfile>,
    pub(super) bootstrap_limits: Option<BootstrapLimits>,
    /// When to ask next; `None` once a whole generation was delivered.
    ask_at: Option<Instant>,
    /// Asks since a whole generation was last delivered.
    asks: u32,
    asked_at: Option<Instant>,
}

impl ContentAttach {
    /// The attach `subscription` opens; `None` for an event-only one.
    pub(super) fn of(subscription: &ProxySubscription) -> Option<Self> {
        subscription.awaits_snapshot.then(|| {
            let mut content = Self {
                resource_kind: subscription.resource_kind,
                bootstrap_profile: subscription.bootstrap_profile,
                bootstrap_limits: subscription.bootstrap_limits,
                ask_at: None,
                asks: 0,
                asked_at: None,
            };
            content.expect_bootstrap();
            content
        })
    }

    /// Wait for a (further) bootstrap frame, longer after each ask.
    fn expect_bootstrap(&mut self) {
        let patience = BOOTSTRAP_PATIENCE.saturating_mul(1 << self.asks.min(3));
        self.ask_at = Some(Instant::now() + patience);
    }

    pub(super) fn note_delivered(&mut self, frame: &FrameKind) {
        match frame {
            FrameKind::BootstrapReady { .. } => {
                self.ask_at = None;
                self.asks = 0;
            }
            FrameKind::BootstrapBegin { .. }
            | FrameKind::BootstrapChunk { .. }
            | FrameKind::BootstrapTombstone { .. } => self.expect_bootstrap(),
            _ => {}
        }
    }

    /// Ask soon after an overflow, but spaced: one upstream generation
    /// serves every consumer, so one slow consumer must not churn the rest.
    pub(super) fn ask_after_overflow(&mut self) {
        let soon = Instant::now() + Duration::from_millis(250);
        self.ask_at = Some(
            self.asked_at
                .map_or(soon, |asked| soon.max(asked + OVERFLOW_SPACING)),
        );
    }

    fn due(&self, now: Instant) -> bool {
        self.asks < MAX_BOOTSTRAP_ASKS && self.ask_at.is_some_and(|at| at <= now)
    }
}

/// What a lost connection leaves the next one: the content subscribers, and
/// the satellite incarnation whose terminal ids they name.
#[derive(Debug, Default)]
pub(crate) struct RelayResume {
    incarnation: Incarnation,
    subscribers: Vec<(u32, ProxySubscriber)>,
}

impl RelayResume {
    /// Apply a withdrawal made while no connection holds the registry.
    pub(crate) fn withdraw(&mut self, unsubscribe: &Unsubscribe) {
        self.subscribers.retain(|(held, sub)| match *unsubscribe {
            Unsubscribe::Client(client) => sub.client != client,
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
                ..
            } => sub.client != client || *held != terminal || sub.seq >= seq,
        });
    }

    /// Whether the satellite now connected is another process than the one
    /// this link last spoke to: its terminal ids name other panes.
    pub(crate) fn satellite_restarted(&self, now: Incarnation) -> bool {
        self.incarnation.is_some() && self.incarnation != now
    }
}

impl RelaySession {
    /// Take over the content subscribers of the connection before this one.
    /// A satellite that restarted may have reused their ids for other panes,
    /// so they are taken only under the same, known incarnation; the rest
    /// were told the satellite went away.
    pub(crate) fn resume(&mut self, resume: RelayResume) {
        if self.incarnation.is_none() || resume.incarnation != self.incarnation {
            return;
        }
        for (terminal, sub) in resume.subscribers {
            let Some(content) = sub.content else {
                continue;
            };
            if sub.out_tx.is_closed() || sub.consumer_cancel.is_cancelled() {
                continue;
            }
            let client = sub.client;
            let subscription = ProxySubscription {
                terminal,
                client,
                out_tx: sub.out_tx,
                consumer_cancel: sub.consumer_cancel,
                seq: sub.seq,
                awaits_snapshot: true,
                resource_kind: content.resource_kind,
                bootstrap_profile: content.bootstrap_profile,
                bootstrap_limits: content.bootstrap_limits,
            };
            // The new connection may have negotiated another profile.
            if self.subscription_rejection(&subscription).is_some() {
                continue;
            }
            self.explicit_content.insert(terminal);
            self.register_subscriber(subscription);
            if let Some(content) = self
                .subscribers
                .get_mut(&terminal)
                .and_then(|subs| subs.iter_mut().find(|sub| sub.client == client))
                .and_then(|sub| sub.content.as_mut())
            {
                content.ask_at = Some(Instant::now());
            }
        }
    }

    /// Take the content subscribers for the next connection; see
    /// [`Self::resume`].
    pub(super) fn take_resume(&mut self) -> RelayResume {
        RelayResume {
            incarnation: self.incarnation,
            subscribers: self
                .subscribers
                .drain()
                .flat_map(|(terminal, subs)| subs.into_iter().map(move |sub| (terminal, sub)))
                .filter(|(_, sub)| sub.content.is_some() && !sub.retire_after_flush)
                .collect(),
        }
    }

    /// The `ATTACH_RESOURCE` each Terminal owes a subscriber that is due a
    /// fresh bootstrap. One serves every subscriber of the Terminal.
    pub(crate) fn take_bootstrap_asks(&mut self) -> Vec<Vec<u8>> {
        let now = Instant::now();
        let mut due = Vec::new();
        for (terminal, subs) in &mut self.subscribers {
            if self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == *terminal)
            {
                continue;
            }
            let mut ask = false;
            for sub in subs {
                let waiting = matches!(sub.gate, SnapshotGate::AwaitingFirst);
                let Some(content) = sub.content.as_mut().filter(|c| waiting && c.due(now)) else {
                    continue;
                };
                content.asks += 1;
                content.asked_at = Some(now);
                content.expect_bootstrap();
                warn!(satellite = %self.host, terminal, client = ?sub.client, asks = content.asks,
                    "a consumer has no drawable generation; asking the satellite for a fresh one");
                ask = true;
            }
            if ask {
                due.push(*terminal);
            }
        }
        due.into_iter()
            .filter_map(|terminal| {
                // Nobody awaits it: what it delivers is the answer.
                let (reply, _) = oneshot::channel();
                let attach = Command::attach_terminal(ResourceId::local(terminal), None);
                self.enqueue_command(attach, reply, None)
            })
            .collect()
    }
}
