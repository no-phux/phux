//! Keeping every content subscriber drawn, asked for, or told.
//!
//! A subscriber is drawn while its gate is open on the live, ready generation.
//! Otherwise it has a deadline, and at the deadline the hub asks the satellite
//! for a fresh bootstrap or, past a few asks, fails the subscription. Events
//! (an answer, an overflow, a redial) only move the deadline;
//! `RelaySession::settle_undrawn` is the one place that acts on it.

use std::time::{Duration, Instant};

use super::{
    BootstrapLimits, BootstrapProfile, ClientId, Command, CommandResult, DeliveryFailure,
    ErrorCode, FrameKind, MAX_BOOTSTRAP_ASKS, Outbound, ProxySubscriber, ProxySubscription,
    RELAY_COMMAND_TIMEOUT, RelayBootstrapFlow, RelaySession, ResourceId, SatelliteHost,
    SnapshotGate, Unsubscribe, oneshot, warn,
};
use crate::hub::operation_fence::Incarnation;

/// Asks older than this no longer count against a consumer. Longer than the
/// longest wait in [`ContentAttach::hurry`], or the count would start over
/// while it waits.
const ASK_WINDOW: Duration = Duration::from_secs(180);

/// A content subscriber's attach, kept so the hub can repeat it.
#[derive(Debug)]
pub(super) struct ContentAttach {
    resource_kind: Option<phux_protocol::ids::ResourceKind>,
    bootstrap_profile: Option<BootstrapProfile>,
    bootstrap_limits: Option<BootstrapLimits>,
    /// When an undrawn consumer is asked for, or failed.
    pub(super) deadline: Option<Instant>,
    /// Asks this consumer caused within [`ASK_WINDOW`] of each other.
    asks: u32,
    asked_at: Option<Instant>,
}

impl ContentAttach {
    /// The attach `subscription` opens; `None` for an event-only one.
    pub(super) fn of(subscription: &ProxySubscription) -> Option<Self> {
        subscription.awaits_snapshot.then_some(Self {
            resource_kind: subscription.resource_kind,
            bootstrap_profile: subscription.bootstrap_profile,
            bootstrap_limits: subscription.bootstrap_limits,
            deadline: None,
            asks: 0,
            asked_at: None,
        })
    }

    /// Nothing is on its way: ask soon, but later after each recent ask. One
    /// upstream generation serves every consumer, so one that keeps losing
    /// its own must not recut the pane for the rest without end.
    pub(super) fn hurry(&mut self) {
        let wait = Duration::from_millis(250 << (2 * self.asks.min(MAX_BOOTSTRAP_ASKS)));
        self.deadline = Some(Instant::now() + wait);
    }

    /// Spend one ask, if any is left.
    fn spend_ask(&mut self, now: Instant) -> bool {
        if self.asked_at.is_some_and(|asked| now - asked > ASK_WINDOW) {
            self.asks = 0;
        }
        if self.asks >= MAX_BOOTSTRAP_ASKS {
            return false;
        }
        self.asks += 1;
        self.asked_at = Some(now);
        self.deadline = Some(now + RELAY_COMMAND_TIMEOUT);
        true
    }
}

impl ProxySubscriber {
    /// Whether this consumer draws the Terminal's live generation.
    fn drawn(&self, flow: Option<&RelayBootstrapFlow>) -> bool {
        matches!(self.gate, SnapshotGate::Open)
            && flow.is_some_and(|flow| flow.ready && self.is_on(flow))
    }

    /// Whether `flow` is the generation this consumer was begun on.
    fn is_on(&self, flow: &RelayBootstrapFlow) -> bool {
        self.current_generation
            .is_some_and(|(stream, bootstrap, _)| {
                (stream, bootstrap) == (flow.stream_id, flow.outward)
            })
    }
}

/// What a viewer whose pane the hub gave up on is told. The code and every
/// word are the hub's: a satellite's own would let it end the viewer's
/// connection, or speak for another satellite.
pub(super) fn undrawable(host: &SatelliteHost, terminal: u32) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code: ErrorCode::ResourceExhausted,
        message: format!(
            "satellite {host} terminal {terminal}: its screen could not be delivered; open it again"
        ),
    }
}

/// What a lost connection leaves the next one: the content attaches to repeat
/// (and nothing of their delivery state, which would pin mailbox slots), and
/// the satellite incarnation whose terminal ids they name.
#[derive(Debug, Default)]
pub(crate) struct RelayResume {
    incarnation: Incarnation,
    attaches: Vec<ProxySubscription>,
}

impl RelayResume {
    /// Apply a withdrawal made while no connection holds the registry.
    pub(crate) fn withdraw(&mut self, unsubscribe: &Unsubscribe) {
        self.attaches.retain(|held| match *unsubscribe {
            Unsubscribe::Client(client) => held.client != client,
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
                ..
            } => held.client != client || held.terminal != terminal || held.seq >= seq,
        });
    }

    /// Whether the satellite now connected is another process than the one
    /// this link last spoke to: its terminal ids name other panes.
    pub(crate) fn satellite_restarted(&self, now: Incarnation) -> bool {
        self.incarnation.is_some() && self.incarnation != now
    }
}

impl RelaySession {
    /// Take over the content attaches of the connection before this one. A
    /// satellite that restarted may have reused their ids for other panes, so
    /// they are taken only under the same, known incarnation; the rest were
    /// told the satellite went away.
    pub(crate) fn resume(&mut self, resume: RelayResume) {
        if self.incarnation.is_none() || resume.incarnation != self.incarnation {
            return;
        }
        for subscription in resume.attaches {
            let (terminal, client) = (subscription.terminal, subscription.client);
            if subscription.out_tx.is_closed() || subscription.consumer_cancel.is_cancelled() {
                continue;
            }
            // The new connection may have negotiated another profile.
            if self.subscription_rejection(&subscription).is_some() {
                let told = undrawable(&self.host, terminal);
                let _ = subscription.out_tx.try_send(Outbound::Frame(told));
                self.forget_proxy_attach(client, terminal);
                continue;
            }
            self.explicit_content.insert(terminal);
            self.register_subscriber(subscription);
            // The old link's attach authority ended with it; this link
            // grants it again under its own epoch.
            self.remember_proxy_attach(client, terminal);
            for sub in self.subscribers.get_mut(&terminal).into_iter().flatten() {
                if let Some(content) = sub.content.as_mut().filter(|_| sub.client == client) {
                    content.deadline = Some(Instant::now());
                }
            }
        }
    }

    /// Take the content attaches for the next connection; see
    /// [`Self::resume`]. Their delivery state ends here.
    pub(super) fn take_resume(&mut self) -> RelayResume {
        let mut attaches = Vec::new();
        for (terminal, subs) in self.subscribers.drain() {
            for sub in subs {
                let Some(content) = sub.content.filter(|_| !sub.retire_after_flush) else {
                    continue;
                };
                attaches.push(ProxySubscription {
                    terminal,
                    client: sub.client,
                    out_tx: sub.out_tx,
                    consumer_cancel: sub.consumer_cancel,
                    seq: sub.seq,
                    awaits_snapshot: true,
                    resource_kind: content.resource_kind,
                    bootstrap_profile: content.bootstrap_profile,
                    bootstrap_limits: content.bootstrap_limits,
                });
            }
        }
        RelayResume {
            incarnation: self.incarnation,
            attaches,
        }
    }

    /// An attach of `terminal` was answered, after every frame it queued: a
    /// consumer still undrawn gets nothing more from it. A refusal may be
    /// momentary (a resize under the capture), so it is asked again too.
    pub(super) fn attach_answered(&mut self, terminal: u32) {
        let flow = self.bootstrap_flows.get(&terminal);
        let coming = flow.is_some_and(|flow| !flow.ready);
        let another = self.pending.values().any(|p| p.attach == Some(terminal));
        if coming || another {
            return;
        }
        for sub in self.subscribers.get_mut(&terminal).into_iter().flatten() {
            if !sub.drawn(flow)
                && let Some(content) = &mut sub.content
            {
                content.hurry();
            }
        }
    }

    /// The hub's own attach found the terminal gone: it closed while the link
    /// was down.
    pub(super) fn asked_for_a_closed_terminal(&mut self, terminal: u32) {
        self.relay_terminal_closed(
            &ResourceId::local(terminal),
            None,
            phux_protocol::wire::frame::CloseReason::Unknown,
            None,
        );
    }

    /// Give every undrawn content subscriber a deadline, and at it ask for a
    /// fresh bootstrap or fail the subscription. Returns the asks; one serves
    /// every subscriber of its Terminal.
    pub(crate) fn settle_undrawn(&mut self) -> Vec<Vec<u8>> {
        if !self.enforce_bootstrap_flow {
            return Vec::new();
        }
        let now = Instant::now();
        let (mut asks, mut failed) = (Vec::new(), Vec::new());
        for (terminal, subs) in &mut self.subscribers {
            let flow = self.bootstrap_flows.get(terminal);
            let detaching = self
                .pending_detaches
                .values()
                .any(|p| p.terminal == *terminal);
            for sub in subs.iter_mut() {
                let drawn = sub.drawn(flow);
                // Blocked on the consumer itself: its mailbox decides.
                let blocked = matches!(
                    sub.gate,
                    SnapshotGate::Retained { .. } | SnapshotGate::Retiring { .. }
                );
                let arriving = flow.filter(|flow| !flow.ready && sub.is_on(flow));
                let Some(content) = &mut sub.content else {
                    continue;
                };
                if drawn {
                    content.deadline = None;
                    continue;
                }
                if let Some(flow) = arriving {
                    content.deadline = Some(flow.progress_at + RELAY_COMMAND_TIMEOUT);
                }
                let deadline = *content.deadline.get_or_insert(now + RELAY_COMMAND_TIMEOUT);
                if blocked || detaching || now < deadline {
                    continue;
                }
                if content.spend_ask(now) {
                    asks.push(*terminal);
                } else {
                    failed.push((*terminal, sub.client));
                }
                warn!(satellite = %self.host, terminal, client = ?sub.client,
                    "a consumer has nothing to draw; asking for it again or failing it");
            }
        }
        for (terminal, client) in failed {
            self.fail_undrawn(terminal, client);
        }
        asks.dedup();
        asks.into_iter()
            .filter_map(|terminal| {
                let (reply, _) = oneshot::channel::<CommandResult>();
                let attach = Command::attach_terminal(ResourceId::local(terminal), None);
                self.enqueue_command(attach, reply, None, None)
            })
            .collect()
    }

    /// End one consumer's content subscription: it is told, and neither
    /// streams nor types there until it attaches again.
    fn fail_undrawn(&mut self, terminal: u32, client: ClientId) {
        let frame = undrawable(&self.host, terminal);
        for sub in self.subscribers.get_mut(&terminal).into_iter().flatten() {
            if sub.client == client {
                sub.content = None;
                let frame = frame.clone();
                Self::begin_retirement(
                    sub,
                    DeliveryFailure {
                        frame,
                        detach_upstream: true,
                    },
                );
            }
        }
        self.forget_proxy_attach(client, terminal);
    }

    fn remember_proxy_attach(&self, client: ClientId, terminal: u32) {
        if let Some(journal) = &self.journal {
            journal.with_mut(|s| {
                if s.federation()
                    .satellite_link_is_current(&self.host, self.epoch)
                {
                    s.federation_mut().register_satellite_proxy_attach(
                        client,
                        self.host.clone(),
                        terminal,
                    );
                }
            });
        }
    }

    fn forget_proxy_attach(&self, client: ClientId, terminal: u32) {
        if let Some(journal) = &self.journal {
            journal.with_mut(|s| {
                if s.federation()
                    .satellite_link_is_current(&self.host, self.epoch)
                {
                    s.federation_mut()
                        .unregister_satellite_proxy_attach(client, &self.host, terminal);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A consumer whose asks are answered but never draw it runs out of them
    /// on the real clock, each wait served in full.
    #[test]
    fn asks_that_are_answered_but_never_drawn_run_out() {
        let mut attach = ContentAttach {
            resource_kind: None,
            bootstrap_profile: None,
            bootstrap_limits: None,
            deadline: None,
            asks: 0,
            asked_at: None,
        };
        let mut now = Instant::now();
        for _ in 0..MAX_BOOTSTRAP_ASKS {
            assert!(attach.spend_ask(now));
            attach.hurry();
            now += attach.deadline.expect("hurried") - Instant::now();
        }
        assert!(!attach.spend_ask(now), "the last wait outlived ASK_WINDOW");
    }
}
