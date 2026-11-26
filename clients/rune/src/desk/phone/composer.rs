//! Phone drafts belong to one server incarnation and one terminal. Enter
//! edits the draft; only Send submits it, as one acknowledged paste + Enter.
use std::collections::HashMap;

use phux_client_runtime::control::DeliveryOutcome;
use phux_protocol::ids::ResourceId;
use rune::{TextEdit, Tx};

use super::super::{Desk, NoticeKind, Pending};

pub const FIELD: &str = "phone-composer-field";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Owner {
    namespace: u64,
    server: Vec<u8>,
    terminal: ResourceId,
}

impl Owner {
    pub(crate) const fn terminal(&self) -> &ResourceId {
        &self.terminal
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Draft {
    pub edit: TextEdit,
    pub sending: Option<(u64, String)>,
}

impl Draft {
    pub(crate) fn can_send(&self) -> bool {
        self.sending.is_none() && !self.edit.is_composing() && !self.edit.text.trim().is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Composer {
    active: Option<Owner>,
    drafts: HashMap<Owner, Draft>,
}

impl Composer {
    pub(crate) fn draft_mut(&mut self, owner: &Owner) -> Option<&mut Draft> {
        self.drafts.get_mut(owner)
    }
}

impl Desk {
    pub(crate) fn composer_owner(&self) -> Option<Owner> {
        if self.native.is_none() || self.modal().is_some() {
            return None;
        }
        let shown = self.connection.shown.as_ref()?;
        if shown.view.is_none()
            || self.is_document(&shown.resource)
            || !matches!(shown.resource, ResourceId::Local { .. })
        {
            return None;
        }
        Some(Owner {
            namespace: self.connection.namespace(),
            server: self.connection.view_server.clone()?,
            terminal: shown.resource.clone(),
        })
    }

    pub(crate) fn active_composer(&self) -> Option<&Owner> {
        let owner = self.phone.as_ref()?.composer.active.as_ref()?;
        (self.composer_owner().as_ref() == Some(owner)).then_some(owner)
    }

    pub(crate) fn composer_draft(&self, owner: &Owner) -> Option<&Draft> {
        self.phone.as_ref()?.composer.drafts.get(owner)
    }

    pub(crate) fn toggle_composer(&mut self, tx: &mut Tx) {
        if self.active_composer().is_some() {
            self.close_composer(tx);
        } else {
            self.open_composer(tx);
        }
    }

    fn open_composer(&mut self, tx: &mut Tx) {
        let Some(owner) = self.composer_owner() else {
            return;
        };
        let Some(phone) = &mut self.phone else { return };
        phone.release();
        phone
            .composer
            .drafts
            .entry(owner.clone())
            .or_insert_with(|| Draft {
                edit: TextEdit::new(""),
                sending: None,
            });
        phone.composer.active = Some(owner);
        self.connection.preedit = None;
        tx.focus_when_shown(FIELD, false);
    }

    pub(crate) fn close_composer(&mut self, tx: &mut Tx) {
        self.retire_composer();
        tx.blur();
    }

    pub(crate) fn retire_composer(&mut self) {
        let Some(phone) = &mut self.phone else { return };
        let Some(owner) = phone.composer.active.take() else {
            return;
        };
        if let Some(draft) = phone.composer.draft_mut(&owner) {
            draft.edit.focused = false;
            draft.edit.cancel_composition();
        }
    }

    pub(crate) fn composer_needs_close(&self) -> bool {
        self.phone
            .as_ref()
            .is_some_and(|p| p.composer.active.is_some())
            && self.active_composer().is_none()
    }

    pub(crate) fn send_composer(&mut self, tx: &mut Tx) {
        let Some(owner) = self.active_composer().cloned() else {
            return;
        };
        self.submit_composer(&owner, false);
        tx.instant();
    }

    pub(in crate::desk) fn confirm_composer(&mut self, owner: &Owner) {
        if self.composer_owner().as_ref() == Some(owner) {
            self.submit_composer(owner, true);
        } else {
            self.say(NoticeKind::Error, "Terminal changed. Your draft was kept.");
        }
    }

    fn submit_composer(&mut self, owner: &Owner, confirmed: bool) {
        let Some(draft) = self.composer_draft(owner) else {
            return;
        };
        if !draft.can_send() {
            return;
        }
        if draft.edit.text.contains('\x1b') {
            self.say(
                NoticeKind::Error,
                "Draft kept. Remove the escape characters before sending.",
            );
            return;
        }
        if !confirmed && draft.edit.text.contains(['\n', '\r']) {
            self.pending = Some(Pending::SendDraft(owner.clone()));
            return;
        }
        let text = draft.edit.text.clone();
        let Some(delivery) = self.enqueue_composer(owner, &text, confirmed) else {
            self.say(
                NoticeKind::Error,
                "Connection changed. Your draft was kept.",
            );
            return;
        };
        if let Some(draft) = self
            .phone
            .as_mut()
            .and_then(|p| p.composer.draft_mut(owner))
        {
            draft.sending = Some((delivery, text));
        }
    }

    fn enqueue_composer(&self, owner: &Owner, text: &str, confirmed: bool) -> Option<u64> {
        let client = self.connection.client.as_ref()?;
        let epoch = self.connection.view_epoch;
        let delivery = client.with_control(|control| {
            if control.connection_epoch() != epoch
                || control.server().map(|server| &server.id) != Some(&owner.server)
            {
                return None;
            }
            let delivery = if confirmed && control.input_ready(&owner.terminal) {
                control.apply_line(&owner.terminal, text)
            } else {
                control.apply_composed_line(&owner.terminal, text)
            };
            Some(delivery)
        })?;
        client.wake();
        client.nudge();
        Some(delivery)
    }

    pub(in crate::desk) fn composer_delivered(
        &mut self,
        delivery: u64,
        outcome: DeliveryOutcome,
        message: &str,
    ) -> bool {
        let namespace = self.connection.namespace();
        let Some((_, draft)) = self.phone.as_mut().and_then(|phone| {
            phone.composer.drafts.iter_mut().find(|(owner, draft)| {
                owner.namespace == namespace
                    && draft
                        .sending
                        .as_ref()
                        .is_some_and(|(id, _)| *id == delivery)
            })
        }) else {
            return false;
        };
        let Some((_, sent)) = draft.sending.take() else {
            return false;
        };
        match outcome {
            DeliveryOutcome::Delivered => {
                if draft.edit.text == sent && !draft.edit.is_composing() {
                    let focused = draft.edit.focused;
                    draft.edit = TextEdit::new("");
                    draft.edit.focused = focused;
                }
            }
            DeliveryOutcome::Refused => {
                self.say(
                    NoticeKind::Error,
                    format!("Draft kept. Send refused: {message}"),
                );
            }
            DeliveryOutcome::Unknown => self.say(
                NoticeKind::Error,
                "Draft kept. Check the terminal before sending again.",
            ),
        }
        true
    }
}
