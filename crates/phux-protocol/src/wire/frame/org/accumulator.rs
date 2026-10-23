//! The shared field accumulator every organization body decodes into, and the common field writers.

use super::{
    ActivationId, ActorRef, ArrangementDoc, ArrangementKey, ArrangementOwner, DecodeError, Decoder,
    Encoder, ExecState, ExitPolicy, LaunchMode, LaunchSpec, LeaseEpoch, LiveTarget,
    MAX_CREDENTIAL_ID_BYTES, MAX_ORG_ATTRS, MAX_ORG_BATCH, MAX_ORG_CURSOR_BYTES,
    MAX_ORG_NAME_BYTES, MAX_SOCKET_PATH_BYTES, MoveItem, OperationId, OrgFilter, Outcome,
    RecordRevision, ResourceId, ResourceKey, ResourceKind, Revision, ServerInstance, SessionId,
    SessionKey, SessionLifetime, WorkspaceKey, bounded_str, decode_actor_ref,
    decode_arrangement_doc, decode_filter, decode_launch, decode_live_target, decode_move_item,
    decode_outcome, decode_pair, decode_pin, decode_terminal_id, encode_actor_ref, enum_byte,
    field, key_bytes, push_bounded, read_key, read_operation, read_u8, read_u32, read_u64, req,
    write_key,
};

fn decode_diagnostic(value: &[u8]) -> Result<crate::api::LaunchDiagnostic, DecodeError> {
    if value.len() > 128 {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    serde_json::from_slice(value).map_err(|_| DecodeError::UnexpectedEof)
}

/// Every `field::org` field a body carried. A body's decoder picks the ones
/// it defines; an unknown id is skipped, so bodies stay additive.
#[derive(Default)]
pub(super) struct Fields {
    pub(super) operation: Option<OperationId>,
    pub(super) session: Option<SessionKey>,
    pub(super) resource: Option<ResourceKey>,
    pub(super) expected: Option<Revision>,
    pub(super) name: Option<String>,
    pub(super) lifetime: Option<u8>,
    pub(super) attrs: Vec<(String, String)>,
    pub(super) launch: Option<Box<LaunchSpec>>,
    pub(super) kind: Option<ResourceKind>,
    pub(super) on_exit: Option<u8>,
    pub(super) from: Option<SessionKey>,
    pub(super) from_revision: Option<Revision>,
    pub(super) to: Option<SessionKey>,
    pub(super) to_revision: Option<Revision>,
    pub(super) target: Option<LiveTarget>,
    pub(super) mode: Option<u8>,
    pub(super) activation: Option<ActivationId>,
    pub(super) arrangement: Option<ArrangementKey>,
    pub(super) document: Option<ArrangementDoc>,
    pub(super) filter: Option<OrgFilter>,
    pub(super) page_size: Option<u32>,
    pub(super) cursor: Option<Vec<u8>>,
    pub(super) undo: Option<OperationId>,
    pub(super) move_items: Vec<MoveItem>,
    pub(super) pins: Vec<RecordRevision>,
    pub(super) revision: Option<Revision>,
    pub(super) live_session: Option<SessionId>,
    pub(super) live_resource: Option<ResourceId>,
    pub(super) state: Option<u8>,
    pub(super) outcome: Option<Outcome>,
    pub(super) diagnostic: Option<crate::api::LaunchDiagnostic>,
    pub(super) persistence_pending: bool,
    pub(super) epoch: Option<LeaseEpoch>,
    pub(super) holder: Option<ActorRef>,
    pub(super) awaiting: Option<bool>,
    pub(super) instance: Option<ServerInstance>,
    pub(super) members: Option<u32>,
    pub(super) workspace: Option<WorkspaceKey>,
    pub(super) ttl_secs: Option<u32>,
    pub(super) credential: Option<String>,
    pub(super) socket_path: Option<String>,
    pub(super) expires_at: Option<u64>,
    pub(super) launch_mode: Option<u8>,
    pub(super) project: Option<String>,
    pub(super) placement: Option<Box<super::CreatePlacement>>,
    pub(super) entry: Option<[u8; 16]>,
    pub(super) replayed: bool,
    pub(super) authority: Option<super::AuthorityId>,
}

impl Fields {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per field id of the shared organization id space"
    )]
    pub(super) fn read(value: &[u8]) -> Result<Self, DecodeError> {
        Self::read_from(&mut Decoder::new(value))
    }

    /// Read fields from `dec` to the end of its body.
    pub(super) fn read_from(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let mut out = Self::default();
        while let Some((id, value)) = dec.read_field()? {
            match id {
                field::org::OPERATION => out.operation = Some(read_operation(value)?),
                field::org::SESSION => out.session = Some(read_key(value, SessionKey::new)?),
                field::org::RESOURCE => out.resource = Some(read_key(value, ResourceKey::new)?),
                field::org::EXPECTED => out.expected = Some(Revision::new(read_u64(value)?)),
                field::org::NAME => out.name = Some(bounded_str(value, MAX_ORG_NAME_BYTES)?),
                field::org::AUTHORITY => {
                    out.authority = Some(read_key(value, super::AuthorityId::new)?);
                }
                field::org::PLACEMENT => {
                    out.placement = Some(Box::new(super::placement::decode(value)?));
                }
                field::org::ENTRY => out.entry = Some(key_bytes(value)?),
                field::org::REPLAYED => out.replayed = Decoder::new(value).read_bool("replayed")?,
                field::org::PROJECT => {
                    out.project = Some(bounded_str(value, MAX_ORG_NAME_BYTES)?);
                }
                field::org::LIFETIME => out.lifetime = Some(read_u8(value)?),
                field::org::ATTR => {
                    let pair = decode_pair(value, MAX_ORG_NAME_BYTES)?;
                    push_bounded(&mut out.attrs, pair, MAX_ORG_ATTRS)?;
                }
                field::org::LAUNCH => out.launch = Some(Box::new(decode_launch(value)?)),
                field::org::KIND => out.kind = Some(ResourceKind::from_wire(read_u8(value)?)),
                field::org::ON_EXIT => out.on_exit = Some(read_u8(value)?),
                field::org::LAUNCH_MODE => out.launch_mode = Some(read_u8(value)?),
                field::org::FROM => out.from = Some(read_key(value, SessionKey::new)?),
                field::org::FROM_REVISION => {
                    out.from_revision = Some(Revision::new(read_u64(value)?));
                }
                field::org::TO => out.to = Some(read_key(value, SessionKey::new)?),
                field::org::TO_REVISION => out.to_revision = Some(Revision::new(read_u64(value)?)),
                field::org::TARGET => out.target = Some(decode_live_target(value)?),
                field::org::MODE => out.mode = Some(read_u8(value)?),
                field::org::ACTIVATION => {
                    out.activation = Some(ActivationId::new(read_u64(value)?));
                }
                field::org::ARRANGEMENT => {
                    out.arrangement = Some(read_key(value, ArrangementKey::new)?);
                }
                field::org::DOCUMENT => out.document = Some(decode_arrangement_doc(value)?),
                field::org::FILTER => out.filter = Some(decode_filter(value)?),
                field::org::PAGE_SIZE => out.page_size = Some(read_u32(value)?),
                field::org::CURSOR => {
                    if value.len() > MAX_ORG_CURSOR_BYTES {
                        return Err(DecodeError::OrganizationLimitExceeded);
                    }
                    out.cursor = Some(value.to_vec());
                }
                field::org::UNDO => out.undo = Some(read_operation(value)?),
                field::org::MOVE_ITEM => {
                    push_bounded(&mut out.move_items, decode_move_item(value)?, MAX_ORG_BATCH)?;
                }
                field::org::PIN => push_bounded(&mut out.pins, decode_pin(value)?, MAX_ORG_BATCH)?,
                field::org::REVISION => out.revision = Some(Revision::new(read_u64(value)?)),
                field::org::LIVE_SESSION => {
                    out.live_session = Some(SessionId::new(read_u32(value)?));
                }
                field::org::LIVE_RESOURCE => {
                    out.live_resource = Some(decode_terminal_id(&mut Decoder::new(value))?);
                }
                field::org::STATE => out.state = Some(read_u8(value)?),
                field::org::OUTCOME => out.outcome = Some(decode_outcome(value)?),
                field::org::LAUNCH_DIAGNOSTIC => out.diagnostic = Some(decode_diagnostic(value)?),
                field::org::PERSISTENCE_PENDING => {
                    out.persistence_pending =
                        Decoder::new(value).read_bool("persistence_pending")?;
                }
                field::org::EPOCH => out.epoch = Some(LeaseEpoch::new(read_u64(value)?)),
                field::org::HOLDER => {
                    out.holder = Some(decode_actor_ref(&mut Decoder::new(value))?);
                }
                field::org::AWAITING => {
                    out.awaiting = Some(Decoder::new(value).read_bool("awaiting")?);
                }
                field::org::INSTANCE => {
                    out.instance = Some(ServerInstance::new(key_bytes(value)?));
                }
                field::org::MEMBERS => out.members = Some(read_u32(value)?),
                field::org::WORKSPACE => {
                    out.workspace = Some(read_key(value, WorkspaceKey::new)?);
                }
                field::org::TTL_SECS => out.ttl_secs = Some(read_u32(value)?),
                field::org::CREDENTIAL => {
                    out.credential = Some(bounded_str(value, MAX_CREDENTIAL_ID_BYTES)?);
                }
                field::org::SOCKET_PATH => {
                    out.socket_path = Some(bounded_str(value, MAX_SOCKET_PATH_BYTES)?);
                }
                field::org::EXPIRES_AT => out.expires_at = Some(read_u64(value)?),
                _ => {}
            }
        }
        Ok(out)
    }

    pub(super) fn operation(&self) -> Result<OperationId, DecodeError> {
        req(self.operation)
    }

    pub(super) fn session(&self) -> Result<SessionKey, DecodeError> {
        req(self.session)
    }

    pub(super) fn resource(&self) -> Result<ResourceKey, DecodeError> {
        req(self.resource)
    }

    pub(super) fn expected(&self) -> Result<Revision, DecodeError> {
        req(self.expected)
    }

    /// An arrangement's owner: exactly one of `SESSION` and `WORKSPACE`.
    pub(super) const fn owner(&self) -> Result<ArrangementOwner, DecodeError> {
        match (self.session, self.workspace) {
            (Some(session), None) => Ok(ArrangementOwner::Session(session)),
            (None, Some(workspace)) => Ok(ArrangementOwner::Workspace(workspace)),
            (None, None) => Err(DecodeError::UnexpectedEof),
            (Some(_), Some(_)) => Err(DecodeError::InvalidArrangement),
        }
    }

    pub(super) fn lifetime(&self) -> Result<Option<SessionLifetime>, DecodeError> {
        enum_byte("SessionLifetime", self.lifetime, SessionLifetime::from_u8)
    }

    pub(super) fn on_exit(&self) -> Result<Option<ExitPolicy>, DecodeError> {
        enum_byte("ExitPolicy", self.on_exit, ExitPolicy::from_u8)
    }

    pub(super) fn launch_mode(&self) -> Result<Option<LaunchMode>, DecodeError> {
        enum_byte("LaunchMode", self.launch_mode, LaunchMode::from_u8)
    }

    pub(super) fn state(&self) -> Result<Option<ExecState>, DecodeError> {
        enum_byte("ExecState", self.state, ExecState::from_u8)
    }
}

pub(super) fn write_operation(enc: &mut Encoder<'_>, operation: &OperationId) {
    write_key(enc, field::org::OPERATION, operation.as_bytes());
}

pub(super) fn write_session(enc: &mut Encoder<'_>, field_id: u32, key: &SessionKey) {
    write_key(enc, field_id, key.as_bytes());
}

pub(super) fn write_resource(enc: &mut Encoder<'_>, key: &ResourceKey) {
    write_key(enc, field::org::RESOURCE, key.as_bytes());
}

pub(super) fn write_name(enc: &mut Encoder<'_>, name: &str) {
    enc.write_field(field::org::NAME, name.as_bytes());
}

pub(super) fn write_project(enc: &mut Encoder<'_>, project: Option<&str>) {
    if let Some(project) = project {
        enc.write_field(field::org::PROJECT, project.as_bytes());
    }
}

pub(super) fn write_actor(enc: &mut Encoder<'_>, field_id: u32, actor: &ActorRef) {
    enc.write_field_with(field_id, |e| encode_actor_ref(actor, e));
}
