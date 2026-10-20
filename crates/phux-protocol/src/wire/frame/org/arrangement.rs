//! The positional arrangement-document codec.

use super::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, DecodeError, Decoder, Encoder, EntryRef,
    EntryTarget, MAX_ARRANGEMENT_BYTES, MAX_ARRANGEMENT_DEPTH, MAX_ENTRY_HOST_BYTES,
    MAX_ORG_NAME_BYTES, ResourceKey, SplitDirection, TARGET_MEMBER, TARGET_SATELLITE,
    bounded_dec_str, field,
};

pub(super) const NODE_LEAF: u8 = 0;

pub(super) const NODE_SPLIT: u8 = 1;

/// Write an arrangement document positionally: `u16 group_count`, then per
/// group `id: bytes16 || name: optional<str> || node`; a node is `0x00 ||
/// entry: bytes16 || target` or `0x01 || dir: u8 || ratio: f32 ||
/// node || node`; a target is `0x00 || resource: bytes16` (a member) or `0x01 ||
/// host: str || id: u32` (a satellite resource).
pub(in crate::wire) fn encode_arrangement_doc(doc: &ArrangementDoc, enc: &mut Encoder<'_>) {
    enc.write_u16_be(u16::try_from(doc.groups.len()).unwrap_or(u16::MAX));
    for group in &doc.groups {
        for byte in &group.id {
            enc.write_u8(*byte);
        }
        enc.write_option(group.name.as_deref(), Encoder::write_str);
        encode_node(&group.tree, enc);
    }
}

pub(super) fn encode_node(node: &ArrangementNode, enc: &mut Encoder<'_>) {
    match node {
        ArrangementNode::Leaf(entry) => {
            enc.write_u8(NODE_LEAF);
            for byte in &entry.entry {
                enc.write_u8(*byte);
            }
            match &entry.target {
                EntryTarget::Member(resource) => {
                    enc.write_u8(TARGET_MEMBER);
                    for byte in resource.as_bytes() {
                        enc.write_u8(*byte);
                    }
                }
                EntryTarget::Satellite { host, id } => {
                    enc.write_u8(TARGET_SATELLITE);
                    enc.write_str(host.as_str());
                    enc.write_u32_be(*id);
                }
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

/// Read an arrangement document, refusing one over
/// [`MAX_ARRANGEMENT_BYTES`], nested deeper than [`MAX_ARRANGEMENT_DEPTH`],
/// with a ratio outside `(0, 1)`, or with trailing bytes.
pub(in crate::wire) fn decode_arrangement_doc(value: &[u8]) -> Result<ArrangementDoc, DecodeError> {
    if value.len() > MAX_ARRANGEMENT_BYTES {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    let mut dec = Decoder::new(value);
    let count = usize::from(dec.read_u16_be()?);
    let mut groups = dec.bounded_capacity(count);
    for _ in 0..count {
        let mut id = [0; 16];
        for byte in &mut id {
            *byte = dec.read_u8()?;
        }
        let name = dec.read_option("Option<str> tag", |d| {
            bounded_dec_str(d, MAX_ORG_NAME_BYTES)
        })?;
        let tree = decode_node(&mut dec, 0)?;
        groups.push(ArrangementGroup { id, name, tree });
    }
    if !dec.at_body_end() {
        return Err(DecodeError::InvalidArrangement);
    }
    Ok(ArrangementDoc { groups })
}

pub(super) fn decode_node(
    dec: &mut Decoder<'_>,
    depth: usize,
) -> Result<ArrangementNode, DecodeError> {
    if depth >= MAX_ARRANGEMENT_DEPTH {
        return Err(DecodeError::LayoutTooDeep);
    }
    match dec.read_u8()? {
        NODE_LEAF => {
            let mut entry = [0; 16];
            for byte in &mut entry {
                *byte = dec.read_u8()?;
            }
            let target = match dec.read_u8()? {
                TARGET_MEMBER => {
                    let mut resource = [0; 16];
                    for byte in &mut resource {
                        *byte = dec.read_u8()?;
                    }
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
            Ok(ArrangementNode::Leaf(EntryRef { entry, target }))
        }
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
