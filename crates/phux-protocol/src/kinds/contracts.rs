// -----------------------------------------------------------------------------
// Stream and spawn contracts: what each catalog row says about its kind's
// bootstrap stream and its `SPAWN_RESOURCE` fields. The decoder, the
// classifier, the relay, and the client kernel read these instead of
// matching on kinds.
// -----------------------------------------------------------------------------

/// How a kind's live output reaches a viewer
/// (`docs/resource-kinds.md#delivery-modes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Delivery {
    /// Records, resumed from a cursor or fenced by an explicit gap.
    Log,
    /// Raw bytes; a gap fences the generation and rebootstraps.
    Raw,
    /// Deltas against each viewer's acknowledged reference.
    Replicated,
    /// Each message is a whole newer document; the newest wins.
    WholeDocument,
}

/// The units a kind measures its geometry in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Geometry {
    /// None: `BOOTSTRAP_BEGIN` carries `0 x 0`.
    None,
    /// A cell grid: `BOOTSTRAP_BEGIN` carries it, never `0 x 0`.
    Cells,
    /// The newest image's own pixel size, carried in the image;
    /// `BOOTSTRAP_BEGIN` carries `0 x 0`.
    Pixels,
}

impl Geometry {
    /// Whether `BOOTSTRAP_BEGIN.cols` and `rows` carry it.
    #[must_use]
    pub const fn in_bootstrap(self) -> bool {
        matches!(self, Self::Cells)
    }
}

/// One bootstrap codec a kind streams in.
#[derive(Debug, Clone, Copy)]
pub struct StreamCodec {
    /// The [`BootstrapCodec`] wire tag.
    pub tag: u8,
    /// The spec's name for the codec, e.g. `AGENT_EVENTS_JSONL_V1`.
    pub name: &'static str,
    /// The bit a peer advertises when it may send this codec, if any. A
    /// Terminal codec has none: the connection's profile selects it.
    pub gate: Option<FeatureGate>,
}

/// A kind's bootstrap stream.
#[derive(Debug, Clone, Copy)]
pub struct StreamContract {
    /// The codecs it streams in; a tag belongs to one kind.
    pub codecs: &'static [StreamCodec],
    /// How its live output is delivered.
    pub delivery: &'static [Delivery],
    /// Its geometry's units.
    pub geometry: Geometry,
    /// The bit that admits a resume cursor (`BOOTSTRAP_BEGIN.resume_after_seq`,
    /// an `ATTACH_RESOURCE` cursor), or `None` when it cannot resume.
    pub resume: Option<FeatureGate>,
    /// Whether the stream is the connection's negotiated Terminal profile,
    /// so a connection that selected none at `HELLO` cannot consume it.
    pub needs_terminal_profile: bool,
}

impl StreamContract {
    /// Whether output is delivered as `delivery`.
    #[must_use]
    pub const fn delivers(&self, delivery: Delivery) -> bool {
        let mut index = 0;
        while index < self.delivery.len() {
            if self.delivery[index] as u8 == delivery as u8 {
                return true;
            }
            index += 1;
        }
        false
    }

    /// The codec with wire tag `tag`, if this kind streams it.
    #[must_use]
    pub const fn codec(&self, tag: u8) -> Option<&'static StreamCodec> {
        let mut index = 0;
        while index < self.codecs.len() {
            if self.codecs[index].tag == tag {
                return Some(&self.codecs[index]);
            }
            index += 1;
        }
        None
    }
}

/// Whether a kind's `SPAWN_RESOURCE` names a parent (L1 §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParentRule {
    /// It must name one: it is a projection its parent supervises.
    Required,
    /// It must not: it is a root.
    Forbidden,
}

/// Whether a `SPAWN_RESOURCE` string field may, or must, be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presence {
    /// Refused when present.
    Forbidden,
    /// Either way.
    Optional,
    /// Refused when absent.
    Required,
}

/// What one of `SPAWN_RESOURCE`'s string fields means for a kind.
#[derive(Debug, Clone, Copy)]
pub struct SpawnFacet {
    /// Whether it may or must be present.
    pub presence: Presence,
    /// What it names for this kind.
    pub meaning: &'static str,
    /// Its bound in bytes.
    pub max_bytes: usize,
}

/// A kind's own check of a spawn whose fields passed its presence rules:
/// the field id it returns is refused as required-but-invalid.
pub type SpawnCheck = fn(&SpawnResource) -> Result<(), u32>;

/// A kind's `SPAWN_RESOURCE` field rules (`docs/protocol.md#commands-events-resources`),
/// checked in field order: parent, `provider`, `native_id`, then
/// [`Self::forbidden`], then [`Self::validate`].
#[derive(Debug, Clone, Copy)]
pub struct SpawnContract {
    /// Whether it names a parent.
    pub parent: ParentRule,
    /// Field 13.
    pub provider: SpawnFacet,
    /// Field 14.
    pub native_id: SpawnFacet,
    /// Further `spawn_terminal` field ids it refuses, in check order.
    pub forbidden: &'static [u32],
    /// The kind's own check, after the presence rules.
    pub validate: Option<SpawnCheck>,
}

/// `a == b`, in a const context.
const fn same_kind(a: ResourceKind, b: ResourceKind) -> bool {
    a.as_wire() == b.as_wire()
        && matches!(a, ResourceKind::Unknown { .. }) == matches!(b, ResourceKind::Unknown { .. })
}

/// The catalog row for `kind` in `catalog`.
#[must_use]
pub const fn kind_spec_in(
    catalog: &'static [KindSpec],
    kind: ResourceKind,
) -> Option<&'static KindSpec> {
    let mut index = 0;
    while index < catalog.len() {
        if same_kind(catalog[index].kind, kind) {
            return Some(&catalog[index]);
        }
        index += 1;
    }
    None
}

/// The row in `catalog` whose kind streams codec `tag`.
#[must_use]
pub const fn codec_kind_in(catalog: &'static [KindSpec], tag: u8) -> Option<&'static KindSpec> {
    let mut index = 0;
    while index < catalog.len() {
        if catalog[index].stream.codec(tag).is_some() {
            return Some(&catalog[index]);
        }
        index += 1;
    }
    None
}

/// The row whose kind streams codec `tag`, or `None` for a codec this build
/// does not allocate.
#[must_use]
pub const fn codec_kind(tag: u8) -> Option<&'static KindSpec> {
    codec_kind_in(&KINDS, tag)
}

/// Whether a stream of `kind` needs the connection's Terminal profile. A
/// kind this build does not serve does: nothing vouches otherwise.
#[must_use]
pub const fn needs_terminal_profile(kind: ResourceKind) -> bool {
    match kind_spec(kind) {
        Some(spec) => spec.stream.needs_terminal_profile,
        None => true,
    }
}

/// Whether `kind`'s spawn names a parent. A kind this build does not serve
/// is not a Terminal, so it would.
#[must_use]
pub const fn parent_rule(kind: ResourceKind) -> ParentRule {
    match kind_spec(kind) {
        Some(spec) => spec.spawn.parent,
        None => ParentRule::Required,
    }
}

/// The longest `provider` any served kind accepts: the decoder's bound
/// before it knows the kind.
#[must_use]
pub const fn max_spawn_provider_bytes() -> usize {
    let mut max = 0;
    let mut index = 0;
    while index < KINDS.len() {
        if KINDS[index].spawn.provider.max_bytes > max {
            max = KINDS[index].spawn.provider.max_bytes;
        }
        index += 1;
    }
    max
}

/// The longest `native_id` any served kind accepts.
#[must_use]
pub const fn max_spawn_native_id_bytes() -> usize {
    let mut max = 0;
    let mut index = 0;
    while index < KINDS.len() {
        if KINDS[index].spawn.native_id.max_bytes > max {
            max = KINDS[index].spawn.native_id.max_bytes;
        }
        index += 1;
    }
    max
}

/// A document's `provider` is its content type, a MIME type.
fn validate_document_spawn(spawn: &SpawnResource) -> Result<(), u32> {
    match spawn.provider.as_deref().map(crate::wire::frame::typed_document::validate_content_type) {
        Some(Err(_)) => Err(crate::wire::field::spawn_terminal::PROVIDER),
        _ => Ok(()),
    }
}

use crate::wire::field::spawn_terminal as spawn_field;

/// What a projection's producer runs, which phux does not: the launch
/// fields every projection refuses.
const PROJECTION_LAUNCH_FIELDS: [u32; 7] = [
    spawn_field::COMMAND,
    spawn_field::CWD,
    spawn_field::ENV,
    spawn_field::TERM,
    spawn_field::OWNER_TERMINAL,
    spawn_field::INITIAL_SIZE,
    spawn_field::RETAIN_SECS,
];

/// The launch fields, then `control`: an agent session controls nothing.
const AGENT_SESSION_REFUSED_FIELDS: [u32; 8] = [
    spawn_field::COMMAND,
    spawn_field::CWD,
    spawn_field::ENV,
    spawn_field::TERM,
    spawn_field::OWNER_TERMINAL,
    spawn_field::INITIAL_SIZE,
    spawn_field::RETAIN_SECS,
    spawn_field::CONTROL,
];

/// A `native_id` a projection may carry, meaning `meaning`.
const fn optional_native_id(meaning: &'static str) -> SpawnFacet {
    SpawnFacet {
        presence: Presence::Optional,
        meaning,
        max_bytes: crate::wire::frame::MAX_RESOURCE_NATIVE_ID_BYTES,
    }
}

/// The spawn of a projection under a parent, produced by what `provider`
/// names.
const fn projection_spawn(
    provider: SpawnFacet,
    native_id: &'static str,
    forbidden: &'static [u32],
    validate: Option<SpawnCheck>,
) -> SpawnContract {
    SpawnContract {
        parent: ParentRule::Required,
        provider,
        native_id: optional_native_id(native_id),
        forbidden,
        validate,
    }
}

/// A required `provider` of at most the ordinary bound.
const fn named_producer(meaning: &'static str) -> SpawnFacet {
    SpawnFacet {
        presence: Presence::Required,
        meaning,
        max_bytes: crate::wire::frame::MAX_RESOURCE_PROVIDER_BYTES,
    }
}

/// A whole-document stream in one gated codec, with no resume cursor.
const fn whole_document_stream(codecs: &'static [StreamCodec], geometry: Geometry) -> StreamContract {
    StreamContract {
        codecs,
        delivery: &[Delivery::WholeDocument],
        geometry,
        resume: None,
        needs_terminal_profile: false,
    }
}
