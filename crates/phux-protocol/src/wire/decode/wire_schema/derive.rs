//! Deriving the schema from the codec: field splitting, presence observation, and retired ids.

use super::{
    BTreeMap, BTreeSet, Decoder, FIELD_RS, FieldEntry, FrameEntry, FrameKind, Presence, Sample,
    Schema, UNSET_TYPE, samples,
};

/// One body-level field: its id and where it sits in the frame bytes.
pub(super) struct Tlv {
    pub(super) id: u32,
    pub(super) wire_type: u8,
    pub(super) span: std::ops::Range<usize>,
    pub(super) value: std::ops::Range<usize>,
}

/// Offset of the body after the length prefix and type byte.
pub(super) const BODY_START: usize = 5;

pub(super) fn type_byte(bytes: &[u8]) -> u8 {
    bytes[BODY_START - 1]
}

/// Walk a frame body as TLV, asserting it parses to the last byte.
pub(super) fn fields(bytes: &[u8]) -> Vec<Tlv> {
    let body = &bytes[BODY_START..];
    let mut dec = Decoder::new(body);
    let mut out = Vec::new();
    while !dec.at_body_end() {
        let start = dec.position();
        let id = u32::try_from(dec.read_varint().unwrap()).unwrap();
        let wire_type = dec.read_u8().unwrap();
        let len = usize::try_from(dec.read_varint().unwrap()).unwrap();
        let value_start = dec.position();
        dec.take(len).unwrap();
        out.push(Tlv {
            id,
            wire_type,
            span: BODY_START + start..BODY_START + dec.position(),
            value: BODY_START + value_start..BODY_START + value_start + len,
        });
    }
    out
}

/// `bytes` with one field cut out and the length prefix rewritten.
pub(super) fn without(bytes: &[u8], cut: &std::ops::Range<usize>) -> Vec<u8> {
    let mut out = bytes[..cut.start].to_vec();
    out.extend_from_slice(&bytes[cut.end..]);
    let len = u32::try_from(out.len() - 4).unwrap();
    out[..4].copy_from_slice(&len.to_be_bytes());
    out
}

pub(super) fn decodes(bytes: &[u8]) -> bool {
    FrameKind::decode(bytes).is_ok()
}

/// `module -> [(CONST_NAME, id)]` parsed from `wire/field.rs`.
pub(super) fn field_modules() -> BTreeMap<&'static str, Vec<(&'static str, u32)>> {
    let mut out: BTreeMap<&str, Vec<(&str, u32)>> = BTreeMap::new();
    let mut current = None;
    for line in FIELD_RS.lines() {
        if let Some(name) = line.strip_prefix("pub mod ") {
            let name = name.trim_end_matches(" {");
            out.entry(name).or_default();
            current = Some(name);
        } else if line == "}" {
            current = None;
        } else if let Some(decl) = line.trim().strip_prefix("pub const ") {
            let module = current.expect("field constants live inside a module");
            let (name, id) = decl
                .strip_suffix(';')
                .and_then(|decl| decl.split_once(": u32 = "))
                .unwrap_or_else(|| panic!("unparsed field constant: {line}"));
            out.entry(module)
                .or_default()
                .push((name, id.parse().unwrap()));
        }
    }
    out
}

/// What the samples of one frame show about one field.
pub(super) struct Observed {
    pub(super) present_in_all: bool,
    pub(super) strip_always_fails: bool,
}

/// Presence of each field id the samples of one frame carry.
pub(super) fn observe(samples: &[&Sample]) -> BTreeMap<u32, Observed> {
    let mut out: BTreeMap<u32, Observed> = BTreeMap::new();
    let per_sample: Vec<Vec<Tlv>> = samples.iter().map(|s| fields(&s.bytes)).collect();
    let ids: BTreeSet<u32> = per_sample.iter().flatten().map(|tlv| tlv.id).collect();
    for id in ids {
        let entry = out.entry(id).or_insert(Observed {
            present_in_all: true,
            strip_always_fails: true,
        });
        for (sample, tlvs) in samples.iter().zip(&per_sample) {
            let Some(tlv) = tlvs.iter().find(|tlv| tlv.id == id) else {
                entry.present_in_all = false;
                continue;
            };
            if decodes(&without(&sample.bytes, &tlv.span)) {
                entry.strip_always_fails = false;
            }
        }
    }
    out
}

impl Observed {
    pub(super) const fn presence(&self) -> Presence {
        if self.present_in_all && self.strip_always_fails {
            Presence::Required
        } else {
            Presence::Optional
        }
    }
}

/// Ids in `1..=max` that no field holds: retired, never reused.
pub(super) fn retired(ids: &[u32]) -> Vec<u32> {
    let max = ids.iter().copied().max().unwrap_or(0);
    (1..=max).filter(|id| !ids.contains(id)).collect()
}

/// The schema as the codec defines it, carrying `types` and `description`
/// and each field's declared `type` over from `current`.
pub(super) fn derive(current: &Schema) -> Schema {
    let modules = field_modules();
    let samples = samples();
    let mut by_type: BTreeMap<u8, Vec<&Sample>> = BTreeMap::new();
    for sample in &samples {
        by_type
            .entry(type_byte(&sample.bytes))
            .or_default()
            .push(sample);
    }
    let frames = by_type
        .into_iter()
        .map(|(type_byte, group)| derive_frame(type_byte, &group, &modules, current))
        .collect();
    Schema {
        description: current.description.clone(),
        types: current.types.clone(),
        frames,
    }
}

pub(super) fn derive_frame(
    type_byte: u8,
    samples: &[&Sample],
    modules: &BTreeMap<&str, Vec<(&str, u32)>>,
    current: &Schema,
) -> FrameEntry {
    let name = samples[0].name;
    let module = samples[0].module;
    let consts = if module.is_empty() {
        &[][..]
    } else {
        modules
            .get(module)
            .unwrap_or_else(|| panic!("{name}: no `{module}` module in field.rs"))
    };
    let observed = observe(samples);
    for id in observed.keys() {
        assert!(
            consts.iter().any(|(_, known)| known == id),
            "{name} emits field {id}, which field.rs `{module}` does not name"
        );
    }
    let declared = current.frames.iter().find(|frame| frame.name == name);
    let fields = consts
        .iter()
        .map(|&(field, id)| {
            let seen = observed.get(&id).unwrap_or_else(|| {
                panic!("{name}.{field} (id {id}) appears in no sample; add one that sets it")
            });
            let ty = declared
                .and_then(|frame| frame.fields.iter().find(|f| f.id == id))
                .map_or_else(|| UNSET_TYPE.to_owned(), |f| f.ty.clone());
            FieldEntry {
                id,
                name: field.to_owned(),
                ty,
                presence: seen.presence(),
            }
        })
        .collect::<Vec<_>>();
    let ids: Vec<u32> = fields.iter().map(|field| field.id).collect();
    FrameEntry {
        name: name.to_owned(),
        type_byte,
        retired: retired(&ids),
        fields,
    }
}
