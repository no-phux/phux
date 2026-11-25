//! A distinct local-owner review, never an implicit restore or a permission bool.
use std::sync::mpsc::{self, Receiver, TryRecvError};
use phux_client_runtime::{Target, archived_relaunch::RelaunchReview};
use phux_protocol::wire::frame::{CommandResult, CommandValue};
use super::{Desk, NoticeKind};

#[derive(Default)]
pub struct Review {
    pub document: Option<String>,
    ready: Option<RelaunchReview>,
    preparing: Option<Receiver<Result<RelaunchReview, String>>>,
    approving: Option<Receiver<Result<CommandResult, String>>>,
    socket: Option<std::path::PathBuf>,
}
impl Review {
    pub fn open(&self) -> bool { self.document.is_some() || self.preparing.is_some() || self.approving.is_some() }
    pub fn can_approve(&self) -> bool { self.ready.is_some() }
}

impl Desk {
    pub fn can_review_raw_relaunch(&self) -> bool {
        self.native.is_none() && self.machines.current.is_none() && self.raw_relaunch_target().is_some()
    }

    fn raw_relaunch_target(&self) -> Option<(phux_protocol::ids::ResourceKey, phux_protocol::ids::Revision)> {
        let client = self.connection.client.as_ref()?;
        let key = self.selected_key()?;
        let organization = client.organization();
        organization.members(&key).find(|record| {
            record.kind == phux_protocol::ids::ResourceKind::Terminal && record.live.is_none() && record.launch.is_some()
        }).map(|record| (record.key, record.revision))
    }

    /// Begin inert preparation; only the separate sheet button can approve it.
    pub fn review_raw_relaunch(&mut self) {
        if self.raw_relaunch.open() { return; }
        if !self.can_review_raw_relaunch() {
            return self.say(NoticeKind::Error, "Raw archived execution review requires a stopped resource on this computer's local owner socket.");
        }
        let Some((resource, expected)) = self.raw_relaunch_target() else { return; };
        let socket = match &self.launch.socket { Ok(socket) => socket.clone(), Err(error) => {
            return self.say(NoticeKind::Error, error.clone());
        }};
        self.pending = None;
        let (sender, receiver) = mpsc::channel();
        self.raw_relaunch.socket = Some(socket.clone());
        self.raw_relaunch.preparing = Some(receiver);
        super::worker::spawn(sender, self.waker.clone(), move |send| {
            send(RelaunchReview::prepare(Target::uds(socket), phux_client_runtime::control::mint_operation(), resource, expected));
        });
    }

    pub fn cancel_raw_relaunch(&mut self) {
        if self.raw_relaunch.approving.is_some() {
            return self.say(NoticeKind::Info, "The exact operation was already submitted; waiting for its outcome, not cancelling execution.");
        }
        self.raw_relaunch = Review::default();
    }
    pub fn approve_raw_relaunch(&mut self, reviewed_document: &str) {
        if self.raw_relaunch.document.as_deref() != Some(reviewed_document) {
            return self.say(NoticeKind::Error, "The displayed recipe changed; no stale approval was submitted.");
        }
        if self.native.is_some() || self.machines.current.is_some()
            || self.launch.socket.as_ref().ok() != self.raw_relaunch.socket.as_ref() {
            self.cancel_raw_relaunch();
            return self.say(NoticeKind::Error, "Executing host changed; raw recipe approval was cancelled.");
        }
        let Some(review) = self.raw_relaunch.ready.take() else { return; };
        let Some(document) = self.raw_relaunch.document.clone() else { return; };
        let (sender, receiver) = mpsc::channel();
        self.raw_relaunch.approving = Some(receiver);
        super::worker::spawn(sender, self.waker.clone(), move |send| { send(review.approve(&document)); });
    }

    pub(super) fn pump_raw_relaunch(&mut self) -> bool {
        if let Some(receiver) = &self.raw_relaunch.preparing {
            match receiver.try_recv() {
                Ok(Ok(review)) => {
                    self.raw_relaunch.document = Some(review.document().to_owned());
                    self.raw_relaunch.ready = Some(review);
                    self.raw_relaunch.preparing = None;
                    return true;
                }
                Ok(Err(error)) => { self.cancel_raw_relaunch(); self.say(NoticeKind::Error, error); return true; }
                Err(TryRecvError::Disconnected) => { self.cancel_raw_relaunch(); self.say(NoticeKind::Error, "Raw recipe preparation ended without a proposal; nothing was approved."); return true; }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(receiver) = &self.raw_relaunch.approving {
            match receiver.try_recv() {
                Ok(result) => {
                    self.raw_relaunch = Review::default();
                    match result {
                        Ok(CommandResult::Accepted { .. } | CommandResult::OkWith(CommandValue::Committed(_))) =>
                            self.say(NoticeKind::Info, "Exact raw relaunch admitted; execution is not yet confirmed."),
                        Ok(other) => self.say(NoticeKind::Error, format!("Raw relaunch was not confirmed: {other:?}")),
                        Err(error) => self.say(NoticeKind::Error, error),
                    }
                    return true;
                }
                Err(TryRecvError::Disconnected) => { self.raw_relaunch = Review::default(); self.say(NoticeKind::Error, "Raw relaunch outcome is unknown; reconcile before retrying."); return true; }
                Err(TryRecvError::Empty) => {}
            }
        }
        false
    }
}
