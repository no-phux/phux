//! Additive features after the full second capability word.
/// One feature in the third trailing `HELLO_OK.server_caps` word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
#[non_exhaustive]
pub enum ServerFeatureExt2 {
    /// All archived relaunch entry points enforce current host templates or an
    /// exact ephemeral local-owner recipe decision. Absence forbids replay.
    ArchivedLaunchAdmission = 0x0000_0001,
}
impl ServerFeatureExt2 {
    /// Known features, in bit order.
    pub const ALL: &'static [Self] = &[Self::ArchivedLaunchAdmission];
    /// Capability catalog name.
    #[must_use]
    pub const fn wire_name(self) -> &'static str { match self { Self::ArchivedLaunchAdmission => "ARCHIVED_LAUNCH_ADMISSION" } }
    /// The wire mask, not a bit index.
    #[must_use]
    pub const fn mask(self) -> u32 { self as u32 }
}

/// Known bits of the third trailing server capability word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerFeatureExt2Set(u32);
impl ServerFeatureExt2Set {
    /// No advertised third-word features.
    #[must_use]
    pub const fn new() -> Self { Self(0) }
    /// All known third-word features.
    #[must_use]
    pub const fn all() -> Self { Self(Self::with(ServerFeatureExt2::ALL).0) }
    /// Construct from known features.
    #[must_use]
    pub const fn with(features: &[ServerFeatureExt2]) -> Self {
        let mut bits = 0; let mut i = 0;
        while i < features.len() { bits |= features[i] as u32; i += 1; }
        Self(bits)
    }
    /// Whether the feature is present.
    #[must_use]
    pub const fn contains(self, feature: ServerFeatureExt2) -> bool { self.0 & feature as u32 != 0 }
    /// Whether no known feature is present.
    #[must_use]
    pub const fn is_empty(self) -> bool { self.0 == 0 }
    /// Known wire bits only.
    #[must_use]
    pub const fn as_wire(self) -> u32 { self.0 & Self::all().0 }
    /// Decode known bits and skip future additions.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self { Self(bits & Self::all().0) }
    /// Iterate known features in wire order.
    pub fn iter(self) -> impl Iterator<Item = ServerFeatureExt2> {
        ServerFeatureExt2::ALL.iter().copied().filter(move |feature| self.contains(*feature))
    }
}
