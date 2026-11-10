/// The resource kinds a client can decode (`HELLO` field `RESOURCE_KINDS`).
///
/// On the wire a `u64` with bit `1 << tag` for each kind's wire tag
/// ([`crate::ids::ResourceKind::as_wire`]). A server streams a resource
/// only to a client whose set holds its kind
/// (`docs/compatibility.md#a-newer-server-an-older-client`).
///
/// Bits for kinds this build does not know are kept, not dropped: they
/// are a newer client's claim and cost a server nothing to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceKindSet(u64);

impl ResourceKindSet {
    const KNOWN: u64 = Self::bit(crate::ids::RESOURCE_KIND_TAG_TERMINAL)
        | Self::bit(crate::ids::RESOURCE_KIND_TAG_AGENT_SESSION)
        | Self::bit(crate::ids::RESOURCE_KIND_TAG_ACCESSIBILITY_TREE)
        | Self::bit(crate::ids::RESOURCE_KIND_TAG_PIXEL_SURFACE)
        | Self::bit(crate::ids::RESOURCE_KIND_TAG_DOCUMENT);

    /// What a `HELLO` without `RESOURCE_KINDS` accepts: the five kinds that
    /// existed when the field was added. Clients from 1.0 up to the
    /// `AccessibilityTree`, `PixelSurface` and `Document` releases cannot
    /// decode those three, but they send the same `HELLO` as the ones that can,
    /// so a server cannot tell them apart; withholding the three would
    /// break every current client to protect superseded ones. A kind added
    /// after this field is never in this set.
    pub const LEGACY: Self = Self(Self::KNOWN);

    const fn bit(tag: u8) -> u64 {
        if tag < 64 { 1 << tag } else { 0 }
    }

    /// The empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Every kind this build knows.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// The set of `kinds`.
    #[must_use]
    pub const fn with(kinds: &[crate::ids::ResourceKind]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < kinds.len() {
            bits |= Self::bit(kinds[i].as_wire());
            i += 1;
        }
        Self(bits)
    }

    /// Whether the set holds `kind`. A tag past bit 63 is never held.
    #[must_use]
    pub const fn contains(self, kind: crate::ids::ResourceKind) -> bool {
        self.0 & Self::bit(kind.as_wire()) != 0
    }

    /// Whether every kind in `other` is in this set.
    #[must_use]
    pub const fn includes(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The wire bits, unknown ones included.
    #[must_use]
    pub const fn as_wire(self) -> u64 {
        self.0
    }

    /// Decode wire bits as they are.
    #[must_use]
    pub const fn from_wire(bits: u64) -> Self {
        Self(bits)
    }
}

impl Default for ResourceKindSet {
    fn default() -> Self {
        Self::all()
    }
}
