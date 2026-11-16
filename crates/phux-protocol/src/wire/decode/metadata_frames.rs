//! Event subscriptions and journaled events. The L3 metadata frames are
//! generated from their `field` modules.

use super::{
    DecodeError, Decoder, FrameKind, ResourceId, decode_actor_ref, decode_agent_event,
    decode_idempotency_key, decode_terminal_id, field, req,
};

impl Decoder<'_> {
    pub(super) fn decode_subscribe_events(&mut self) -> Result<FrameKind, DecodeError> {
        use field::subscribe_events as f;
        let mut terminal: Option<ResourceId> = None;
        let mut after_seq: Option<u64> = None;
        for_each_field!(self, value, {
            f::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
            f::AFTER_SEQ => after_seq = Some(sub!(value, Decoder::read_u64_be)),
        });
        Ok(FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        })
    }

    pub(super) fn decode_event(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal: Option<ResourceId> = None;
        let mut event: Option<crate::wire::frame::AgentEvent> = None;
        let mut stamp = StampParts::default();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::event::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                field::event::EVENT => event = Some(sub!(value, decode_agent_event)),
                other => stamp.absorb(other, value)?,
            }
        }
        Ok(FrameKind::Event {
            terminal,
            event: req(event)?,
            stamp: stamp.finish(),
        })
    }
}

/// The `EVENT` journal-stamp fields (3-6, ADR-0123); the stamp exists iff
/// `seq` was present.
#[derive(Default)]
pub(super) struct StampParts {
    pub(super) seq: Option<u64>,
    pub(super) ts_ms: u64,
    pub(super) actor: Option<crate::wire::frame::ActorRef>,
    pub(super) operation_id: Option<crate::ids::IdempotencyKey>,
}

impl StampParts {
    /// Take one `EVENT` field if it is a stamp field; ignore any other id.
    pub(super) fn absorb(&mut self, id: u32, value: &[u8]) -> Result<(), DecodeError> {
        match id {
            field::event::SEQ => self.seq = Some(sub!(value, Decoder::read_u64_be)),
            field::event::TS_MS => self.ts_ms = sub!(value, Decoder::read_u64_be),
            field::event::ACTOR => self.actor = Some(sub!(value, decode_actor_ref)),
            field::event::OPERATION_ID => self.operation_id = Some(decode_idempotency_key(value)?),
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Option<Box<crate::wire::frame::EventStamp>> {
        let seq = self.seq?;
        Some(Box::new(
            crate::wire::frame::EventStamp::new(seq, self.ts_ms)
                .with_actor(self.actor)
                .with_operation_id(self.operation_id),
        ))
    }
}
