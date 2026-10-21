//! Positional codecs of the session, window, and resource rows and the resource-facet list.

use super::{
    AgentFacet, DecodeError, Decoder, Encoder, LAYOUT_TAG_LEAF, LAYOUT_TAG_SPLIT, ResourceInfo,
    ResourceKind, ResourceLifecycle, SessionId, SessionInfo, WindowId, WindowInfo, decode_list_len,
    decode_option_str, decode_option_terminal_id, decode_option_window_id, decode_terminal_id,
    encode_list_len, encode_option_str, encode_option_terminal_id, encode_option_window_id,
    encode_terminal_id,
};

/// Maximum layout-tree depth the decoder follows while skipping the retired
/// `WindowInfo` layout slot before [`DecodeError::LayoutTooDeep`], so
/// hostile nesting cannot overflow the stack.
pub const MAX_LAYOUT_DEPTH: usize = 64;

/// Skip a tree in the retired `WindowInfo` layout slot (`docs/spec/L1.md`
/// §1.1): a sender writes the slot absent, and a receiver discards a present
/// tree after walking it to its end, bounding depth at [`MAX_LAYOUT_DEPTH`].
pub(super) fn skip_layout_tree(dec: &mut Decoder<'_>, depth: usize) -> Result<(), DecodeError> {
    if depth >= MAX_LAYOUT_DEPTH {
        return Err(DecodeError::LayoutTooDeep);
    }
    match dec.read_u8()? {
        LAYOUT_TAG_LEAF => decode_terminal_id(dec).map(drop),
        LAYOUT_TAG_SPLIT => {
            let _dir = dec.read_u8()?;
            let _ratio = dec.read_f32_be()?;
            skip_layout_tree(dec, depth + 1)?;
            skip_layout_tree(dec, depth + 1)
        }
        other => Err(DecodeError::unknown_enum("LayoutNode", other)),
    }
}

pub(in crate::wire) fn encode_session_info(info: &SessionInfo, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| encode_session_info_body(info, enc));
}

pub(super) fn encode_session_info_body(info: &SessionInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(info.id.get());
    enc.write_str(&info.name);
    encode_option_window_id(info.active_window, enc);
    enc.write_i64_be(info.created_at_unix_secs);
    enc.write_u16_be(info.window_count);
    enc.write_u16_be(info.attached_client_count);
}

pub(in crate::wire) fn decode_session_info(
    dec: &mut Decoder<'_>,
) -> Result<SessionInfo, DecodeError> {
    dec.read_record(decode_session_info_body)
}

pub(super) fn decode_session_info_body(dec: &mut Decoder<'_>) -> Result<SessionInfo, DecodeError> {
    let id = SessionId::new(dec.read_u32_be()?);
    let name = dec.read_str()?.to_owned();
    let active_window = decode_option_window_id(dec)?;
    let created_at_unix_secs = dec.read_i64_be()?;
    let window_count = dec.read_u16_be()?;
    let attached_client_count = dec.read_u16_be()?;
    Ok(SessionInfo {
        id,
        name,
        active_window,
        created_at_unix_secs,
        window_count,
        attached_client_count,
        // Not positional: these ride the trailing session facets and the
        // extension block.
        keep_empty: false,
        lifetime: None,
        key: None,
        attrs: Vec::new(),
        recent_input: None,
    })
}

pub(in crate::wire) fn encode_window_info(info: &WindowInfo, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| encode_window_info_body(info, enc));
}

pub(super) fn encode_window_info_body(info: &WindowInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(info.id.get());
    enc.write_u32_be(info.session_id.get());
    enc.write_u16_be(info.index);
    enc.write_str(&info.name);
    encode_option_terminal_id(info.active_resource.as_ref(), enc);
    // The retired layout slot, always written absent.
    enc.write_option(None::<()>, |_, ()| {});
}

pub(in crate::wire) fn decode_window_info(
    dec: &mut Decoder<'_>,
) -> Result<WindowInfo, DecodeError> {
    dec.read_record(decode_window_info_body)
}

pub(super) fn decode_window_info_body(dec: &mut Decoder<'_>) -> Result<WindowInfo, DecodeError> {
    let id = WindowId::new(dec.read_u32_be()?);
    let session_id = SessionId::new(dec.read_u32_be()?);
    let index = dec.read_u16_be()?;
    let name = dec.read_str()?.to_owned();
    let active_resource = decode_option_terminal_id(dec)?;
    dec.read_option("Option<LayoutNode> tag", |d| skip_layout_tree(d, 0))?;
    Ok(WindowInfo {
        id,
        session_id,
        index,
        name,
        active_resource,
    })
}

pub(in crate::wire) fn encode_terminal_info(info: &ResourceInfo, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| encode_terminal_info_body(info, enc));
}

pub(super) fn encode_terminal_info_body(info: &ResourceInfo, enc: &mut Encoder<'_>) {
    encode_terminal_id(&info.id, enc);
    enc.write_u32_be(info.window_id.get());
    enc.write_u16_be(info.cols);
    enc.write_u16_be(info.rows);
    encode_option_str(info.title.as_deref(), enc);
    encode_option_str(info.cwd.as_deref(), enc);
}

pub(in crate::wire) fn decode_terminal_info(
    dec: &mut Decoder<'_>,
) -> Result<ResourceInfo, DecodeError> {
    dec.read_record(decode_terminal_info_body)
}

pub(super) fn decode_terminal_info_body(
    dec: &mut Decoder<'_>,
) -> Result<ResourceInfo, DecodeError> {
    let id = decode_terminal_id(dec)?;
    let window_id = WindowId::new(dec.read_u32_be()?);
    let cols = dec.read_u16_be()?;
    let rows = dec.read_u16_be()?;
    let title = decode_option_str(dec)?.map(str::to_owned);
    let cwd = decode_option_str(dec)?.map(str::to_owned);
    Ok(ResourceInfo {
        id,
        window_id,
        cols,
        rows,
        title,
        cwd,
        kind: ResourceKind::Terminal,
        parent: None,
        agent: None,
        // Not positional: these ride the snapshot extension block.
        lifecycle: ResourceLifecycle::Running,
        exit: None,
        input_holder: None,
        viewers: Vec::new(),
        session: None,
    })
}

/// Session-facet flag bit: the session is keep-empty (ADR-0105).
pub(super) const SESSION_FACET_KEEP_EMPTY: u8 = 0x01;

/// Write the trailing resource-facet list, or nothing when it is empty and no
/// later element (`more_follow`) needs it as a positional anchor.
pub(super) fn encode_resource_facets(
    resources: &[ResourceInfo],
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    let rows = resources.iter().filter(|p| p.has_resource_facets()).count();
    if rows == 0 && !more_follow {
        return;
    }
    encode_list_len(rows, enc);
    for pane in resources.iter().filter(|p| p.has_resource_facets()) {
        enc.write_record(|enc| {
            encode_terminal_id(&pane.id, enc);
            enc.write_u8(pane.kind.as_wire());
            encode_option_terminal_id(pane.parent.as_ref(), enc);
            enc.write_option(pane.agent.as_ref(), |e, agent| {
                e.write_str(&agent.provider);
                encode_option_str(agent.native_id.as_deref(), e);
                e.write_str(&agent.state);
            });
        });
    }
}

/// Read the trailing resource-facet list if the enclosing field has bytes
/// left, joining each row onto its `resources` entry by id.
pub(super) fn decode_resource_facets(
    dec: &mut Decoder<'_>,
    resources: &mut [ResourceInfo],
) -> Result<(), DecodeError> {
    if dec.at_body_end() {
        return Ok(());
    }
    let rows = decode_list_len(dec)?;
    for _ in 0..rows {
        dec.read_record(|dec| apply_resource_facet(dec, resources))?;
    }
    Ok(())
}

/// Join one resource-facet row onto its `resources` entry (ignored when
/// none).
pub(super) fn apply_resource_facet(
    dec: &mut Decoder<'_>,
    resources: &mut [ResourceInfo],
) -> Result<(), DecodeError> {
    let id = decode_terminal_id(dec)?;
    let kind = ResourceKind::from_wire(dec.read_u8()?);
    let parent = decode_option_terminal_id(dec)?;
    let agent = dec.read_option("Option<AgentFacet> tag", |d| {
        Ok(AgentFacet {
            provider: d.read_str()?.to_owned(),
            native_id: decode_option_str(d)?.map(str::to_owned),
            state: d.read_str()?.to_owned(),
        })
    })?;
    if let Some(pane) = resources.iter_mut().find(|p| p.id == id) {
        pane.kind = kind;
        pane.parent = parent;
        pane.agent = agent;
    }
    Ok(())
}
