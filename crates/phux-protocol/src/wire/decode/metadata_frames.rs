//! L3 metadata frame bodies, event subscriptions, and journaled events.

use super::{
    DecodeError, Decoder, FrameKind, ResourceId, Scope, decode_actor_ref, decode_agent_event,
    decode_idempotency_key, decode_metadata_scope_key, decode_scope, decode_terminal_id, field,
    req, utf8_value,
};

impl Decoder<'_> {
    pub(super) fn decode_get_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        })
    }

    pub(super) fn decode_set_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        let mut value_bytes: Vec<u8> = Vec::new();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::set_metadata::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::set_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::set_metadata::KEY => key = Some(utf8_value(value)?),
                field::set_metadata::VALUE => value_bytes = value.to_vec(),
                _ => {}
            }
        }
        Ok(FrameKind::SetMetadata {
            request_id,
            scope: req(scope)?,
            key: req(key)?,
            value: value_bytes,
        })
    }

    pub(super) fn decode_delete_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::DeleteMetadata {
            request_id,
            scope,
            key,
        })
    }

    pub(super) fn decode_list_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut scope: Option<Scope> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::list_metadata::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::list_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                _ => {}
            }
        }
        Ok(FrameKind::ListMetadata {
            request_id,
            scope: req(scope)?,
        })
    }

    pub(super) fn decode_subscribe_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::subscribe_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::subscribe_metadata::KEY => key = Some(utf8_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::SubscribeMetadata {
            scope: req(scope)?,
            key: req(key)?,
        })
    }

    pub(super) fn decode_metadata_changed(&mut self) -> Result<FrameKind, DecodeError> {
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        let mut value_bytes: Option<Vec<u8>> = None;
        let mut actor = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_changed::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::metadata_changed::KEY => key = Some(utf8_value(value)?),
                field::metadata_changed::VALUE => value_bytes = Some(value.to_vec()),
                field::metadata_changed::ACTOR => actor = Some(sub!(value, decode_actor_ref)),
                _ => {}
            }
        }
        Ok(FrameKind::MetadataChanged {
            scope: req(scope)?,
            key: req(key)?,
            value: value_bytes,
            actor,
        })
    }

    pub(super) fn decode_metadata_value(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut value_bytes: Option<Vec<u8>> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_value::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::metadata_value::VALUE => value_bytes = Some(value.to_vec()),
                _ => {}
            }
        }
        Ok(FrameKind::MetadataValue {
            request_id,
            value: value_bytes,
        })
    }

    pub(super) fn decode_metadata_keys(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut keys: Vec<String> = Vec::new();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_keys::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::metadata_keys::KEYS => {
                    let mut d = Decoder::new(value);
                    let count = d.read_u32_be()?;
                    let count_usize =
                        usize::try_from(count).map_err(|_| DecodeError::LengthOverflow)?;
                    let mut out = d.bounded_capacity(count_usize);
                    for _ in 0..count_usize {
                        out.push(d.read_str()?.to_owned());
                    }
                    keys = out;
                }
                _ => {}
            }
        }
        Ok(FrameKind::MetadataKeys { request_id, keys })
    }

    pub(super) fn decode_subscribe_events(&mut self) -> Result<FrameKind, DecodeError> {
        use field::subscribe_events as f;
        let mut terminal: Option<ResourceId> = None;
        let mut after_seq: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                f::AFTER_SEQ => after_seq = Some(sub!(value, Decoder::read_u64_be)),
                _ => {}
            }
        }
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
