//! The positional arrangement-document codec.

use super::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, DecodeError, Decoder, Encoder, EntryRef,
    EntryTarget, MAX_ARRANGEMENT_BYTES, MAX_ARRANGEMENT_DEPTH, MAX_ENTRY_HOST_BYTES,
    MAX_ORG_NAME_BYTES, ResourceKey, SplitDirection, TARGET_MEMBER, TARGET_SATELLITE,
    bounded_dec_str, field,
};

pub(super) const NODE_LEAF: u8 = 0;

pub(super) const NODE_SPLIT: u8 = 1;

/// The trailer after a leaf's target that makes it a stack.
const LEAF_STACK: u8 = 1;

/// The flag after the last group: the writer knows stacks.
const DOC_STACKS: u8 = 1;

/// Write an arrangement document positionally: `u16 group_count`, then per
/// group a record `id: bytes16 || name: optional<str> || node || [locked:
/// u8]`, the trailing byte present only when the group is locked; a node is a
/// record `0x00 || entry: bytes16 || target` or `0x01 || dir: u8 || ratio:
/// f32 || node || node`; a target is `0x00 || resource: bytes16` (a member)
/// or `0x01 || host: str || id: u32` (a satellite resource). A stack is the
/// leaf for its shown entry followed by `0x01 || active: u16 || count: u16 ||
/// count × (entry: bytes16 || target)`, every entry in strip order, so an
/// older peer reads the shown entry alone. After the groups, `flags: u8`
/// (bit 0: the writer knows stacks) when any is set. A record is `u32`
/// length then body, so groups and nodes can grow at their ends.
pub(in crate::wire) fn encode_arrangement_doc(doc: &ArrangementDoc, enc: &mut Encoder<'_>) {
    enc.write_u16_be(u16::try_from(doc.groups.len()).unwrap_or(u16::MAX));
    for group in &doc.groups {
        enc.write_record(|enc| {
            enc.write_raw(&group.id);
            enc.write_option(group.name.as_deref(), Encoder::write_str);
            encode_node(&group.tree, enc);
            if group.locked {
                enc.write_u8(1);
            }
        });
    }
    if doc.stacks {
        enc.write_u8(DOC_STACKS);
    }
}

pub(super) fn encode_node(node: &ArrangementNode, enc: &mut Encoder<'_>) {
    enc.write_record(|enc| encode_node_body(node, enc));
}

pub(super) fn encode_node_body(node: &ArrangementNode, enc: &mut Encoder<'_>) {
    match node {
        ArrangementNode::Leaf(entry) => {
            enc.write_u8(NODE_LEAF);
            encode_entry(entry, enc);
        }
        ArrangementNode::Stack { active, entries } => {
            enc.write_u8(NODE_LEAF);
            // A stack the decoder would refuse can't be written; the shown
            // entry alone stands in for it, as an older peer reads it.
            let Some(shown) = entries.get(usize::from(*active)) else {
                return;
            };
            encode_entry(shown, enc);
            enc.write_u8(LEAF_STACK);
            enc.write_u16_be(*active);
            enc.write_u16_be(u16::try_from(entries.len()).unwrap_or(u16::MAX));
            for entry in entries.iter().take(usize::from(u16::MAX)) {
                encode_entry(entry, enc);
            }
        }
        ArrangementNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            enc.write_u8(NODE_SPLIT);
            enc.write_u8(dir.to_u8());
            enc.write_f32_be(*ratio);
            encode_node(first, enc);
            encode_node(second, enc);
        }
    }
}

fn encode_entry(entry: &EntryRef, enc: &mut Encoder<'_>) {
    enc.write_raw(&entry.entry);
    match &entry.target {
        EntryTarget::Member(resource) => {
            enc.write_u8(TARGET_MEMBER);
            enc.write_raw(resource.as_bytes());
        }
        EntryTarget::Satellite { host, id } => {
            enc.write_u8(TARGET_SATELLITE);
            enc.write_str(host.as_str());
            enc.write_u32_be(*id);
        }
    }
}

fn decode_entry(dec: &mut Decoder<'_>) -> Result<EntryRef, DecodeError> {
    let entry: [u8; 16] = dec.read_array()?;
    let target = match dec.read_u8()? {
        TARGET_MEMBER => {
            let resource: [u8; 16] = dec.read_array()?;
            EntryTarget::Member(
                ResourceKey::new(resource).ok_or(DecodeError::InvalidOrganizationKey)?,
            )
        }
        TARGET_SATELLITE => {
            let host = dec.read_str()?;
            if host.is_empty() || host.len() > MAX_ENTRY_HOST_BYTES {
                return Err(DecodeError::InvalidArrangement);
            }
            let host = crate::ids::SatelliteHost::new(host);
            EntryTarget::Satellite {
                host,
                id: dec.read_u32_be()?,
            }
        }
        other => return Err(DecodeError::unknown_enum("EntryTarget", other)),
    };
    Ok(EntryRef { entry, target })
}

/// The stack after a leaf, or the leaf alone. A trailer this peer doesn't
/// know is skipped, as an older peer skips this one.
fn decode_leaf(dec: &mut Decoder<'_>) -> Result<ArrangementNode, DecodeError> {
    let shown = decode_entry(dec)?;
    if dec.at_body_end() || dec.read_u8()? != LEAF_STACK {
        return Ok(ArrangementNode::Leaf(shown));
    }
    let active = dec.read_u16_be()?;
    let count = usize::from(dec.read_u16_be()?);
    let mut entries = dec.bounded_capacity(count);
    for _ in 0..count {
        entries.push(decode_entry(dec)?);
    }
    if count < 2 || entries.get(usize::from(active)) != Some(&shown) {
        return Err(DecodeError::InvalidArrangement);
    }
    Ok(ArrangementNode::Stack { active, entries })
}

/// Read an arrangement document, refusing one over
/// [`MAX_ARRANGEMENT_BYTES`], nested deeper than [`MAX_ARRANGEMENT_DEPTH`],
/// or with a ratio outside `(0, 1)`. Bytes a newer peer appended after the
/// last group, or at the end of a group or node record, are skipped.
pub(in crate::wire) fn decode_arrangement_doc(value: &[u8]) -> Result<ArrangementDoc, DecodeError> {
    if value.len() > MAX_ARRANGEMENT_BYTES {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    let mut dec = Decoder::new(value);
    let count = usize::from(dec.read_u16_be()?);
    let mut groups = dec.bounded_capacity(count);
    for _ in 0..count {
        groups.push(dec.read_record(decode_group)?);
    }
    let stacks = !dec.at_body_end() && dec.read_u8()? & DOC_STACKS != 0;
    Ok(ArrangementDoc { groups, stacks })
}

pub(super) fn decode_group(dec: &mut Decoder<'_>) -> Result<ArrangementGroup, DecodeError> {
    let id: [u8; 16] = dec.read_array()?;
    let name = dec.read_option("Option<str> tag", |d| {
        bounded_dec_str(d, MAX_ORG_NAME_BYTES)
    })?;
    let tree = decode_node(dec, 0)?;
    let locked = !dec.at_body_end() && dec.read_u8()? != 0;
    Ok(ArrangementGroup {
        id,
        name,
        tree,
        locked,
    })
}

pub(super) fn decode_node(
    dec: &mut Decoder<'_>,
    depth: usize,
) -> Result<ArrangementNode, DecodeError> {
    if depth >= MAX_ARRANGEMENT_DEPTH {
        return Err(DecodeError::LayoutTooDeep);
    }
    dec.read_record(|dec| decode_node_body(dec, depth))
}

pub(super) fn decode_node_body(
    dec: &mut Decoder<'_>,
    depth: usize,
) -> Result<ArrangementNode, DecodeError> {
    match dec.read_u8()? {
        NODE_LEAF => decode_leaf(dec),
        NODE_SPLIT => {
            let raw_dir = dec.read_u8()?;
            let dir = SplitDirection::from_u8(raw_dir)
                .ok_or_else(|| DecodeError::unknown_enum("SplitDirection", raw_dir))?;
            let ratio = dec.read_f32_be()?;
            if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
                return Err(DecodeError::InvalidArrangement);
            }
            let first = Box::new(decode_node(dec, depth + 1)?);
            let second = Box::new(decode_node(dec, depth + 1)?);
            Ok(ArrangementNode::Split {
                dir,
                ratio,
                first,
                second,
            })
        }
        other => Err(DecodeError::unknown_enum("ArrangementNode", other)),
    }
}

pub(super) fn write_document(enc: &mut Encoder<'_>, doc: &ArrangementDoc) {
    enc.write_field_with(field::org::DOCUMENT, |e| encode_arrangement_doc(doc, e));
}
