//! Server-held approvals in the Inbox (ADR-0128).
//!
//! The runtime projects the server's `phux.approval/v1/<id>` records; each
//! pending one is an Inbox entry with Approve and Deny. An entry stands
//! exactly while the server lists the approval: deciding it here, deciding
//! it elsewhere, an expiry, or a withdrawal all take it down. A decision is
//! sent once. A stale one (the approval had already ended) is reported, not
//! retried.

use std::collections::{HashMap, HashSet};

use gpui::{App, Context};
use phux_client_runtime::approvals::{Approval, Decision, DecisionOutcome};
use phux_protocol::ids::ApprovalId;

use super::desk::Desk;
use super::notifications::{Kind, SystemTag};
use crate::connection::ApprovalOutcome;

/// What the Inbox knows about approvals beyond the feed.
#[derive(Debug, Default)]
pub(crate) struct ApprovalInbox {
    /// Decisions sent and not yet resolved, with what each one decides.
    deciding: HashMap<ApprovalId, String>,
}

impl ApprovalInbox {
    /// Whether a decision on `id` is in flight.
    pub(crate) fn deciding(&self, id: ApprovalId) -> bool {
        self.deciding.contains_key(&id)
    }
}

/// What the held action does, for a person: `kill`, `interrupt signal`.
fn action(approval: &Approval) -> String {
    match (approval.method.as_str(), approval.signal.as_deref()) {
        (_, Some(signal)) => format!("{signal} signal"),
        ("KILL_RESOURCE", None) => "kill".to_owned(),
        ("DETACH_CLIENT", None) => "forced detach".to_owned(),
        (method, None) => method.to_ascii_lowercase().replace('_', " "),
    }
}

/// Who asked: the name it announced, else its credential, else its client.
fn requester(approval: &Approval) -> String {
    let who = &approval.requester;
    who.client_name
        .clone()
        .or_else(|| who.credential_id.clone())
        .unwrap_or_else(|| format!("client {}", who.client))
}

pub(crate) fn title(approval: &Approval) -> String {
    format!("Approve {}?", action(approval))
}

pub(crate) fn body(approval: &Approval) -> String {
    let subjects = if approval.subjects.is_empty() {
        "the server".to_owned()
    } else {
        approval.subjects.join(", ")
    };
    format!("{} asks · {subjects}", requester(approval))
}

impl Desk {
    /// Fold the pending set into the Inbox: post new approvals, and take
    /// down every entry the server no longer lists, with its system
    /// notification.
    pub(super) fn note_approvals(&mut self, cx: &App) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let pending = link.connection.approvals.clone();
        let live: HashSet<ApprovalId> = pending
            .iter()
            .filter_map(|approval| ApprovalId::parse(&approval.id))
            .collect();
        let ended: Vec<ApprovalId> = self
            .feed
            .items()
            .filter_map(|entry| entry.approval)
            .filter(|id| !live.contains(id))
            .collect();
        self.feed.retain(|entry| {
            entry.kind != Kind::Approval || entry.approval.is_some_and(|id| live.contains(&id))
        });
        for id in ended {
            cx.dismiss_system_notification(&SystemTag::Approval(id).to_string());
        }
        for approval in &pending {
            let Some(id) = ApprovalId::parse(&approval.id) else {
                continue;
            };
            // Idempotent: the feed keeps one entry per approval.
            self.feed.post_approval(title(approval), body(approval), id);
        }
    }

    /// Report each resolved decision once, as a typed notification.
    pub(super) fn note_approval_outcomes(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for outcome in link.connection.take_approval_outcomes() {
            self.report_approval(outcome);
        }
    }

    fn report_approval(&mut self, outcome: ApprovalOutcome) {
        let ApprovalOutcome {
            id,
            decision,
            outcome,
        } = outcome;
        let what = self
            .approvals
            .deciding
            .remove(&id)
            .unwrap_or_else(|| "the held action".to_owned());
        let verb = match decision {
            Decision::Approve => "Approved",
            Decision::Deny => "Denied",
        };
        match outcome {
            DecisionOutcome::Decided => self.post(Kind::Info, &format!("{verb} {what}"), None),
            DecisionOutcome::Stale => self.post(
                Kind::Info,
                "Approval already decided",
                Some(format!(
                    "The {what} was decided elsewhere, expired, or was withdrawn before your decision arrived. Nothing was sent again."
                )),
            ),
            DecisionOutcome::Refused { message, .. } => self.post(
                Kind::Error,
                &format!("Could not {} the {what}", decision.as_str()),
                Some(message),
            ),
            DecisionOutcome::Unconfirmed => self.post(
                Kind::Error,
                "Approval decision unconfirmed",
                Some(format!(
                    "The connection ended before the server confirmed the {what}. Check the Inbox before deciding again."
                )),
            ),
        }
    }

    /// Send one decision from an Inbox entry. Nothing is retried.
    pub(crate) fn decide_approval(
        &mut self,
        id: ApprovalId,
        decision: Decision,
        cx: &mut Context<Self>,
    ) {
        let sent = self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.decide_approval(id, decision));
        if sent {
            let what = self
                .link
                .as_ref()
                .and_then(|link| {
                    link.connection
                        .approvals
                        .iter()
                        .find(|approval| ApprovalId::parse(&approval.id) == Some(id))
                        .map(action)
                })
                .unwrap_or_else(|| "held action".to_owned());
            self.approvals.deciding.insert(id, what);
        } else {
            self.post(
                Kind::Refusal,
                "Decision not sent",
                Some(
                    "The server is not connected, or this approval is already being decided."
                        .into(),
                ),
            );
        }
        cx.notify();
    }

    /// The connection is gone: so are its approvals.
    pub(super) fn forget_approvals(&mut self) {
        self.approvals = ApprovalInbox::default();
        self.feed.retain(|entry| entry.kind != Kind::Approval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_runtime::approvals::Requester;

    fn approval(method: &str, signal: Option<&str>, name: Option<&str>) -> Approval {
        Approval {
            id: "07".repeat(16),
            requester: Requester {
                client: 9,
                credential_id: None,
                client_name: name.map(str::to_owned),
            },
            method: method.to_owned(),
            signal: signal.map(str::to_owned),
            subjects: vec!["terminal:4".to_owned()],
            requested_at_ms: 1,
            expires_at_ms: 2,
        }
    }

    #[test]
    fn an_entry_names_the_action_the_requester_and_the_subject() {
        let kill = approval("KILL_RESOURCE", None, Some("agent"));
        assert_eq!(title(&kill), "Approve kill?");
        assert_eq!(body(&kill), "agent asks · terminal:4");
        let signal = approval("SIGNAL_TERMINAL", Some("interrupt"), None);
        assert_eq!(title(&signal), "Approve interrupt signal?");
        assert_eq!(body(&signal), "client 9 asks · terminal:4");
        assert_eq!(title(&approval("UPGRADE", None, None)), "Approve upgrade?");
    }
}
