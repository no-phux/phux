//! Asking the satellite for a fresh bootstrap on a consumer's behalf.
//!
//! After a redial, after a slow consumer's delivery overflowed, and when an
//! attach it answered delivered none. Every path ends drawn, or in an error.

use std::time::{Duration, Instant};

use super::{
    BootstrapLimits, BootstrapProfile, Command, CommandResult, DeliveryFailure, ErrorCode,
    FrameKind, MAX_BOOTSTRAP_ASKS, Outbound, ProxySubscriber, ProxySubscription,
    RELAY_COMMAND_TIMEOUT, RelaySession, ResourceId, SnapshotGate, Unsubscribe, oneshot, warn,
};
use crate::hub::operation_fence::Incarnation;

/// Asks older than this no longer count against a consumer.
const ASK_WINDOW: Duration = Duration::from_secs(60);

/// A content subscriber's attach, kept so the hub can repeat it.
#[derive(Debug)]
pub(super) struct ContentAttach {
    resource_kind: Option<phux_protocol::ids::ResourceKind>,
    bootstrap_profile: Option<BootstrapProfile>,
    bootstrap_limits: Option<BootstrapLimits>,
    /// When to ask next; `None` while nothing is owed.
    pub(super) ask_at: Option<Instant>,
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
            ask_at: None,
            asks: 0,
            asked_at: None,
        })
    }

    /// Owe this consumer a fresh bootstrap, later after each recent one: the
    /// upstream generation serves every consumer, so a slow one must not recut
    /// the pane for the rest without end. `false` once it has had its share.
    pub(super) fn owe_bootstrap(&mut self) -> bool {
        let now = Instant::now();
        if self.asked_at.is_some_and(|asked| now - asked > ASK_WINDOW) {
            self.asks = 0;
        }
        if self.asks >= MAX_BOOTSTRAP_ASKS {
            return false;
        }
        self.ask_at = Some(now + Duration::from_millis(250 << (2 * self.asks)));
        true
    }

    /// A whole generation was delivered: nothing is owed.
    pub(super) const fn drawn(&mut self) {
        self.ask_at = None;
    }
}

impl ProxySubscriber {
    /// Whether this consumer holds a content attach with nothing to draw.
    const fn undrawn(&self) -> bool {
        self.content.is_some() && matches!(self.gate, SnapshotGate::AwaitingFirst)
    }

    /// End the subscription with an error, delivered like any retirement.
    pub(super) fn fail(&mut self, code: ErrorCode, message: String) {
        RelaySession::begin_retirement(
            self,
            DeliveryFailure {
                frame: FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                },
                detach_upstream: true,
            },
        );
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

/// A fresh-bootstrap attach the hub sent, awaiting the satellite's answer.
#[derive(Debug)]
pub(super) struct BootstrapAsk {
    terminal: u32,
    pub(super) deadline: Instant,
    reply: oneshot::Receiver<CommandResult>,
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
            if let Some((code, message)) = self.subscription_rejection(&subscription) {
                let refused = FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                };
                let _ = subscription.out_tx.try_send(Outbound::Frame(refused));
                continue;
            }
            self.explicit_content.insert(terminal);
            self.register_subscriber(subscription);
            for sub in self.subscribers.get_mut(&terminal).into_iter().flatten() {
                if let Some(content) = sub.content.as_mut().filter(|_| sub.client == client) {
                    content.ask_at = Some(Instant::now());
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
        self.bootstrap_asks.clear();
        RelayResume {
            incarnation: self.incarnation,
            attaches,
        }
    }

    /// An attach of `terminal` was answered, after every frame it queued. A
    /// consumer still with nothing, and no generation on its way, will get
    /// nothing from it: the satellite's writer dropped the bootstrap.
    pub(super) fn attach_answered(&mut self, terminal: u32) {
        let another_in_flight = self.pending.values().any(|pending| {
            pending
                .subscription
                .is_some_and(|rider| rider.0 == terminal)
        });
        let coming = self
            .bootstrap_flows
            .get(&terminal)
            .is_some_and(|f| !f.ready);
        if another_in_flight || coming {
            return;
        }
        for sub in self.subscribers.get_mut(&terminal).into_iter().flatten() {
            if !sub.undrawn() {
                continue;
            }
            let owed = sub
                .content
                .as_mut()
                .is_none_or(ContentAttach::owe_bootstrap);
            if !owed {
                let message = format!(
                    "satellite {} terminal {terminal} answered every attach without a screen; subscription closed",
                    self.host
                );
                sub.fail(ErrorCode::InternalError, message);
            }
        }
    }

    /// Settle the asks the satellite answered, or left unanswered too long.
    fn settle_bootstrap_asks(&mut self, now: Instant) {
        for mut ask in std::mem::take(&mut self.bootstrap_asks) {
            let refusal = match ask.reply.try_recv() {
                Ok(CommandResult::Error { code, message }) => Some((code, message)),
                Ok(_) => {
                    self.attach_answered(ask.terminal);
                    continue;
                }
                Err(oneshot::error::TryRecvError::Empty) if now < ask.deadline => {
                    self.bootstrap_asks.push(ask);
                    continue;
                }
                Err(_) => None,
            };
            let (code, message) = refusal.unwrap_or_else(|| {
                (
                    ErrorCode::SatelliteUnreachable,
                    format!("satellite {} did not answer a fresh attach", self.host),
                )
            });
            if code == ErrorCode::TerminalNotFound {
                // It closed while the link was down.
                self.relay_terminal_closed(
                    &ResourceId::local(ask.terminal),
                    None,
                    phux_protocol::wire::frame::CloseReason::Unknown,
                    None,
                );
                continue;
            }
            for sub in self
                .subscribers
                .get_mut(&ask.terminal)
                .into_iter()
                .flatten()
            {
                if sub.undrawn() {
                    sub.fail(code, message.clone());
                }
            }
        }
    }

    /// The `ATTACH_RESOURCE` each Terminal owes a subscriber that is due a
    /// fresh bootstrap. One serves every subscriber of the Terminal.
    pub(crate) fn take_bootstrap_asks(&mut self) -> Vec<Vec<u8>> {
        let now = Instant::now();
        self.settle_bootstrap_asks(now);
        let mut due: Vec<u32> = Vec::new();
        for (terminal, subs) in &mut self.subscribers {
            let detaching = self
                .pending_detaches
                .values()
                .any(|pending| pending.terminal == *terminal);
            let asked = self.bootstrap_asks.iter().any(|a| a.terminal == *terminal);
            let coming = self.bootstrap_flows.get(terminal).is_some_and(|f| !f.ready);
            if detaching || asked || coming {
                continue;
            }
            for sub in subs.iter_mut().filter(|sub| sub.undrawn()) {
                let Some(content) = &mut sub.content else {
                    continue;
                };
                if content.ask_at.is_none_or(|at| at > now) {
                    continue;
                }
                content.asks += 1;
                content.asked_at = Some(now);
                content.ask_at = None;
                warn!(satellite = %self.host, terminal, client = ?sub.client, asks = content.asks,
                    "a consumer has nothing to draw; asking the satellite for a fresh bootstrap");
                if due.last() != Some(terminal) {
                    due.push(*terminal);
                }
            }
        }
        due.into_iter()
            .filter_map(|terminal| {
                let (reply, answer) = oneshot::channel();
                self.bootstrap_asks.push(BootstrapAsk {
                    terminal,
                    deadline: now + RELAY_COMMAND_TIMEOUT,
                    reply: answer,
                });
                let attach = Command::attach_terminal(ResourceId::local(terminal), None);
                self.enqueue_command(attach, reply, None)
            })
            .collect()
    }
}
