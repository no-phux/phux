//! Connection-bound, memory-only 1Password exchanges. No plaintext is retained here.

use super::ClientId;
use phux_protocol::credentials::{CredentialRequest, SecretValues};
use std::collections::HashMap;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Ready,
    Claimed,
}

#[derive(Debug)]
struct Pending {
    requester: ClientId,
    provider: ClientId,
    request: CredentialRequest,
    phase: Phase,
    deadline: tokio::time::Instant,
    reply: oneshot::Sender<Option<SecretValues>>,
}

#[derive(Debug, Default)]
pub(crate) struct CredentialBroker {
    providers: HashMap<String, ClientId>,
    pending: HashMap<String, Pending>,
}

impl CredentialBroker {
    pub(crate) fn provider(&self, name: &str) -> Option<ClientId> {
        self.providers.get(name).copied()
    }

    pub(crate) fn register(&mut self, name: &str, client: ClientId) -> Result<(), &'static str> {
        if let Some(existing) = self.provider(name) {
            return if existing == client {
                Ok(())
            } else {
                Err("credential provider name is already connected")
            };
        }
        if self.providers.len() >= 32 {
            return Err("too many credential providers");
        }
        self.providers.insert(name.to_owned(), client);
        Ok(())
    }

    pub(crate) fn insert(
        &mut self,
        id: String,
        requester: ClientId,
        provider: ClientId,
        request: CredentialRequest,
        ttl: std::time::Duration,
    ) -> Result<oneshot::Receiver<Option<SecretValues>>, &'static str> {
        if self.pending.len() >= 128
            || self
                .pending
                .values()
                .filter(|p| p.requester == requester)
                .count()
                >= 8
        {
            return Err("too many credential requests");
        }
        let (reply, receiver) = oneshot::channel();
        self.pending.insert(
            id,
            Pending {
                requester,
                provider,
                request,
                phase: Phase::Waiting,
                deadline: tokio::time::Instant::now() + ttl,
                reply,
            },
        );
        Ok(receiver)
    }

    pub(crate) fn ready(&mut self, id: &str) -> bool {
        let Some(pending) = self.pending.get_mut(id) else {
            return false;
        };
        if pending.deadline <= tokio::time::Instant::now() {
            return false;
        }
        pending.phase = Phase::Ready;
        true
    }

    pub(crate) fn claim(
        &mut self,
        provider: ClientId,
        label: &str,
    ) -> Option<(String, CredentialRequest, ClientId)> {
        let now = tokio::time::Instant::now();
        let (id, pending) = self.pending.iter_mut().find(|(_, p)| {
            p.provider == provider
                && p.request.provider == label
                && p.phase == Phase::Ready
                && p.deadline > now
        })?;
        pending.phase = Phase::Claimed;
        Some((id.clone(), pending.request.clone(), pending.requester))
    }

    pub(crate) fn completion_request(
        &mut self,
        id: &str,
        provider: ClientId,
    ) -> Option<(ClientId, CredentialRequest)> {
        if self.pending.get(id)?.deadline <= tokio::time::Instant::now() {
            self.pending.remove(id);
            return None;
        }
        let pending = self.pending.get(id)?;
        (pending.provider == provider && pending.phase == Phase::Claimed)
            .then(|| (pending.requester, pending.request.clone()))
    }

    pub(crate) fn complete(&mut self, id: &str, values: Option<SecretValues>) {
        if let Some(pending) = self.pending.remove(id)
            && pending.deadline > tokio::time::Instant::now()
        {
            let _ = pending.reply.send(values);
        }
    }

    pub(crate) fn remove(&mut self, id: &str) {
        self.pending.remove(id);
    }

    pub(crate) fn disconnect(&mut self, client: ClientId) {
        self.providers.retain(|_, provider| *provider != client);
        self.pending
            .retain(|_, p| p.requester != client && p.provider != client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CredentialRequest {
        CredentialRequest {
            provider: "mac".into(),
            references: vec!["op://dev/item/token".into()],
            purpose: "agent".into(),
        }
    }

    #[test]
    fn only_bound_provider_claims_after_approval_and_completion_is_single_use() {
        let mut broker = CredentialBroker::default();
        broker.register("mac", ClientId(1)).unwrap();
        assert!(broker.register("mac", ClientId(2)).is_err());
        let mut reply = broker
            .insert(
                "id".into(),
                ClientId(3),
                ClientId(1),
                request(),
                std::time::Duration::from_secs(120),
            )
            .unwrap();
        assert!(broker.claim(ClientId(1), "mac").is_none());
        assert!(broker.ready("id"));
        assert!(broker.claim(ClientId(2), "mac").is_none());
        assert!(broker.claim(ClientId(1), "other-label").is_none());
        assert!(broker.claim(ClientId(1), "mac").is_some());
        assert!(broker.claim(ClientId(1), "mac").is_none());
        assert!(broker.completion_request("id", ClientId(2)).is_none());
        broker.complete("id", Some(SecretValues(vec!["secret".into()])));
        assert_eq!(reply.try_recv().unwrap().unwrap().0, vec!["secret"]);
        assert!(broker.completion_request("id", ClientId(1)).is_none());
    }

    #[test]
    fn disconnect_retires_binding_and_every_exchange() {
        let mut broker = CredentialBroker::default();
        broker.register("mac", ClientId(1)).unwrap();
        let mut reply = broker
            .insert(
                "id".into(),
                ClientId(3),
                ClientId(1),
                request(),
                std::time::Duration::from_secs(120),
            )
            .unwrap();
        broker.disconnect(ClientId(1));
        assert!(reply.try_recv().is_err());
        assert!(broker.provider("mac").is_none());
        assert!(broker.pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_is_checked_before_settlement_even_without_a_timeout_task() {
        let mut broker = CredentialBroker::default();
        let mut reply = broker
            .insert(
                "id".into(),
                ClientId(3),
                ClientId(1),
                request(),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
        assert!(broker.ready("id"));
        assert!(broker.claim(ClientId(1), "mac").is_some());
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(broker.completion_request("id", ClientId(1)).is_none());
        assert!(reply.try_recv().is_err());
        assert!(broker.pending.is_empty());
    }
}
