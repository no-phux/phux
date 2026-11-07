//! Successful document subscriptions, bound to the identity at request time.
//! Runtime terminal admission is optimistic while ATTACH_RESOURCE is pending;
//! organization inventory and a retained paint are not subscription receipts.
use super::*;
use phux_protocol::wire::frame::ResourceRecord;

pub(super) struct Subscription {
    epoch: u64,
    record: ResourceRecord,
    request: u32,
    detach_request: Option<u32>,
    accepted: bool,
}

pub(super) fn same_binding(a: &ResourceRecord, b: &ResourceRecord) -> bool {
    a.key == b.key && a.kind == b.kind && a.activation == b.activation
}

impl Connection {
    pub(super) fn forget_old_document_subscriptions(&self) {
        let epoch = self.connection_epoch();
        // The driver can have completed a fresh HELLO before the UI drains
        // its boundary events. Keep requests captured on that fresh epoch.
        self.document_subscriptions
            .borrow_mut()
            .retain(|_, subscription| subscription.epoch == epoch);
    }

    pub(crate) fn subscribe_resource(&self, id: &ResourceId) -> u32 {
        let Some(record) = self
            .resource_record(id)
            .filter(|record| documents::is_document(record.kind))
        else {
            return self.client.attach_terminal_preserving_geometry(id);
        };
        let epoch = self.connection_epoch();
        let mut subscriptions = self.document_subscriptions.borrow_mut();
        if subscriptions.get(id).is_some_and(|subscription| {
            subscription.epoch == epoch && same_binding(&subscription.record, &record)
        }) {
            return 0;
        }
        let replace = subscriptions
            .get(id)
            .is_some_and(|subscription| subscription.epoch == epoch);
        let (detach_request, request) = self.client.with_control(|control| {
            if replace {
                return (Some(control.detach_terminal(id)), 0);
            }
            let request = control.attach_terminal_preserving_geometry(id);
            if request != 0 {
                return (None, request);
            }
            // An automatic subscription may already exist, but did not capture
            // this native binding. Recut only this client's stream to prove it.
            (Some(control.detach_terminal(id)), 0)
        });
        subscriptions.insert(
            id.clone(),
            Subscription {
                epoch,
                record,
                request,
                detach_request,
                accepted: false,
            },
        );
        request
    }

    pub(super) fn note_document_detach(
        &self,
        id: &ResourceId,
        request: u32,
        error: Option<&str>,
        epoch: u64,
    ) {
        if let Some(subscription) = self.document_subscriptions.borrow_mut().get_mut(id)
            && subscription.detach_request == Some(request)
            && subscription.epoch == epoch
            && error.is_none()
        {
            subscription.detach_request = None;
            subscription.request = self.client.attach_terminal_preserving_geometry(id);
        }
    }

    pub(super) fn note_document_subscription(
        &self,
        id: &ResourceId,
        request: u32,
        error: Option<&str>,
        epoch: u64,
    ) {
        if let Some(subscription) = self.document_subscriptions.borrow_mut().get_mut(id)
            && subscription.request == request
            && subscription.epoch == epoch
        {
            subscription.accepted = error.is_none();
        }
    }

    pub(crate) fn document_subscribed(&self, id: &ResourceId) -> bool {
        self.document_subscriptions
            .borrow()
            .get(id)
            .is_some_and(|subscription| subscription.accepted)
            && self.document_request_record(id).is_some()
    }

    /// Return the binding captured by the request, never relabel bytes with a
    /// separately sampled current record after validating the subscription.
    /// The server publishes its bootstrap before the attach receipt. Retain
    /// those bytes while pending; `document_content` still requires admission.
    pub(super) fn document_request_record(&self, id: &ResourceId) -> Option<ResourceRecord> {
        if self.client.status() != Status::Attached {
            return None;
        }
        let record = self.resource_record(id)?;
        self.document_subscriptions
            .borrow()
            .get(id)
            .filter(|subscription| {
                subscription.request != 0
                    && subscription.detach_request.is_none()
                    && subscription.epoch == self.connection_epoch()
                    && same_binding(&subscription.record, &record)
            })
            .map(|subscription| subscription.record.clone())
    }

    pub(crate) fn subscription_ready(&self, id: &ResourceId) -> bool {
        if self.client.status() != Status::Attached || self.status != Status::Attached {
            return false;
        }
        match self.document_kind(id) {
            Some(_) => self.document_subscribed(id),
            None => self.client.input_ready(id),
        }
    }

    pub(crate) fn resource_binding_matches(
        &self,
        id: &ResourceId,
        expected: Option<&ResourceRecord>,
    ) -> bool {
        expected.is_none_or(|expected| {
            self.resource_record(id)
                .is_some_and(|current| same_binding(expected, &current))
        })
    }

    pub(super) fn refresh_document_subscriptions(&self) {
        let changed: Vec<_> = self
            .document_subscriptions
            .borrow()
            .iter()
            .filter_map(|(id, subscription)| {
                let current = self.resource_record(id)?;
                (!same_binding(&subscription.record, &current)).then(|| id.clone())
            })
            .collect();
        for id in changed {
            self.subscribe_resource(&id);
        }
    }
}
