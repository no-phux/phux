//! Projection and acceptance of leased agent publications.
use super::{AgentPublication, AgentRecordMode, Arc, Client, OwnedEffect};

impl Client {
    pub(crate) fn accept_agent_publications(&mut self) {
        let mut control = self.runtime.control();
        for publication in self.agent_receipts.drain(..) {
            control.accept_agent_publication(&publication);
        }
    }

    pub(super) fn agent_publication_effect(&mut self, publication: Arc<AgentPublication>) {
        if publication.mode == AgentRecordMode::Replace {
            // The runtime supersedes its journal and queued events at commit.
            // A leased event may already be staged here until effect_clear;
            // supersede that copy too, without accepting any resume prefix.
            self.owned_effects.retain(|effect| {
                effect.kind != crate::c::types::EFFECT_AGENT_RECORDS
                    || effect.terminal_id != publication.resource_id
                    || effect.detail == crate::c::types::AGENT_RECORDS_CLOSED
            });
            self.agent_receipts
                .retain(|receipt| receipt.resource_id != publication.resource_id);
        }
        self.agent_streams
            .insert(publication.resource_id.clone(), Some(publication.stream));
        if let Some(batch) = crate::projection::agent_records::project(&publication) {
            let detail = match batch.mode {
                AgentRecordMode::Replace => crate::c::types::AGENT_RECORDS_RETAINED,
                AgentRecordMode::Append => crate::c::types::AGENT_RECORDS_LIVE,
            };
            let mut effect = OwnedEffect::simple(
                crate::c::types::EFFECT_AGENT_RECORDS,
                detail,
                publication.resource_id.clone(),
            );
            effect.stream_id = publication.stream.stream_id.get();
            effect.bootstrap_id = publication.stream.bootstrap_id.get();
            effect.seq = batch.seq;
            effect.bytes = batch.jsonl.into_bytes();
            self.owned_effects.push(effect);
        }
        self.agent_receipts.push(publication);
    }
}
