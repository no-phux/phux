//! Capability advertisements (SPEC §6.2).
//!
//! Capabilities live in HELLO and apply for the life of the connection. The
//! types here are wire-level: they appear in [`ClientCapabilities`] /
//! `ServerCapabilities` envelopes and drive the server-side VT byte-stream
//! rewriter per [ADR-0013].
//!
//! Under ADR-0013 the cell-level `StyleColor`
//! downsampling helper is gone; the server rewrites SGR sequences in the
//! outbound byte stream instead (see `phux_server::downsample`). What
//! survives on the protocol side is the *advertised tier itself* —
//! [`ColorSupport`] — which the rewriter consults to decide what to emit.
//!
//! [ADR-0013]: https://github.com/phall1/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md

/// A client's color tier (SPEC §6.2).
///
/// Advertised once at HELLO time; the server rewrites outbound VT bytes to
/// fit. `TrueColor` is the most-permissive tier — clients that have not yet
/// advertised caps default here so we never silently downgrade.
///
/// Variants are ordered from most-permissive to least-permissive, but the
/// enum is `#[non_exhaustive]`: protocol additions (e.g. a future palette
/// negotiation tier) must not break downstream consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorSupport {
    /// 24-bit direct RGB. The server forwards SGR truecolor sequences
    /// (`CSI 38;2;R;G;B m` / `CSI 48;2;R;G;B m`) verbatim.
    #[default]
    TrueColor,
    /// xterm 256-color palette: 16 system colors, a 6x6x6 RGB cube
    /// (indices 16..=231), and 24-step grayscale (232..=255).
    Indexed256,
    /// 16 system colors only (the ANSI base set + 8 bright variants).
    Indexed16,
    /// Monochrome — the renderer cannot distinguish color at all. SGR color
    /// sequences MUST be stripped from the outbound byte stream.
    ///
    /// Currently unused by [`detect_color_support`] (which never returns
    /// `Mono`); reserved for future explicit opt-in via configuration or
    /// for accessibility profiles. Added here so the wire codec has a
    /// stable tag for it.
    Mono,
}

impl ColorSupport {
    /// Wire tag for the [`ColorSupport`] variant.
    ///
    /// Discriminants are stable within the v0.x protocol; new variants
    /// append. Decoders that see an unknown tag MUST fall back to
    /// [`ColorSupport::TrueColor`] (the safe most-permissive default)
    /// rather than reject the frame — `#[non_exhaustive]` is the
    /// load-bearing contract.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::TrueColor => 0,
            Self::Indexed256 => 1,
            Self::Indexed16 => 2,
            Self::Mono => 3,
        }
    }

    /// Inverse of [`Self::as_wire`]. Unknown tags map to `None`; the
    /// decoder applies a default at the call site (typically
    /// [`ColorSupport::TrueColor`]) so a forward-compat HELLO from a
    /// future client never fails to decode.
    #[must_use]
    pub const fn from_wire(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::TrueColor,
            1 => Self::Indexed256,
            2 => Self::Indexed16,
            3 => Self::Mono,
            _ => return None,
        })
    }
}

/// How the server should emit terminal content to this consumer (SPEC §6.2).
///
/// The server has two emitters for a pane's content (see
/// `phux-server::terminal_actor`): the **raw PTY broadcast** — byte-faithful,
/// low-latency, the path interactive shells/TUIs rely on for exact styling —
/// and the **per-consumer synthesized state-sync tick**, which diffs the live
/// grid against a per-consumer reference and ships only the delta. The tick is
/// the right emitter for an agent or remote state-sync consumer that wants a
/// coherent grid model rather than a raw byte stream, but as the human path it
/// adds a visible typing-latency floor and can lose byte-exact styling
/// (phux-yeca). A consumer advertises its preference here at HELLO time; the
/// server honors it per connection.
///
/// `#[non_exhaustive]` and decoded leniently: an unknown wire tag falls back
/// to [`OutputMode::Raw`] (the safe interactive default), so a future mode
/// never fails an older server's decode (phux-fseo).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OutputMode {
    /// Raw PTY byte broadcast. Byte-faithful and low-latency — the default
    /// and the human-TUI path.
    #[default]
    Raw,
    /// Per-consumer synthesized state-sync tick: the server emits grid
    /// deltas with a per-consumer monotonic `seq`. For agent / remote
    /// state-sync consumers (ADR-0018).
    StateSync,
}

impl OutputMode {
    /// Wire tag for the [`OutputMode`] variant. Stable within v0.x; new
    /// variants append.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::Raw => 0,
            Self::StateSync => 1,
        }
    }

    /// Inverse of [`Self::as_wire`]. An unknown tag maps to [`OutputMode::Raw`]
    /// (the safe interactive default) so a forward-compat HELLO from a future
    /// client never fails to decode.
    #[must_use]
    pub const fn from_wire(tag: u8) -> Self {
        match tag {
            1 => Self::StateSync,
            // 0 and any unknown future tag both fall back to Raw.
            _ => Self::Raw,
        }
    }
}

// -----------------------------------------------------------------------------
// Layer / LayerSet — SPEC §6.2 conformance-tier bitset (ADR-0015).
// -----------------------------------------------------------------------------

/// A single conformance tier from SPEC §6.2 / §16.
///
/// L1 (Terminal substrate) is always implied and always implemented; L2
/// (Collection lifecycle) and L3 (Metadata storage) are optional services
/// negotiated via [`LayerSet`] in HELLO / `HELLO_OK`.
///
/// Per ADR-0015 the **negotiated tier set** is the intersection of the
/// client's and server's advertised layers. Out-of-tier messages MUST
/// surface as protocol errors (SPEC §16.4).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Layer {
    /// Terminal substrate. Always implemented; always implied.
    L1 = 0x01,
    /// Collection lifecycle (OPTIONAL). SPEC §7.3 / §11.L2.
    L2 = 0x02,
    /// Metadata storage (OPTIONAL). SPEC §7.4 / §11.L3.
    L3 = 0x04,
}

/// A bit-field of [`Layer`]s. Wire encoding: a single `u8` carrying the
/// OR of the variants' raw discriminants.
///
/// Construction goes through [`Self::new`] / [`Self::with`] / [`Self::insert`]
/// so the L1-always-on invariant is preserved. Direct field-literal
/// construction is intentionally NOT supported — `Layer` may grow with
/// future tiers and the bitset must remain forward-compat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerSet(u8);

impl LayerSet {
    /// The L1-only set. Equivalent to `LayerSet::default()`.
    ///
    /// L1 is always implied per SPEC §6.2; the bit is always present in
    /// the wire encoding regardless of construction path.
    #[must_use]
    pub const fn new() -> Self {
        Self(Layer::L1 as u8)
    }

    /// Build a set containing all listed layers (plus the always-on L1).
    #[must_use]
    pub const fn with(layers: &[Layer]) -> Self {
        let mut bits = Layer::L1 as u8;
        let mut i = 0;
        while i < layers.len() {
            bits |= layers[i] as u8;
            i += 1;
        }
        Self(bits)
    }

    /// The full set: L1 + L2 + L3. Used by the reference TUI which
    /// advertises every tier it speaks (SPEC §16.3).
    #[must_use]
    pub const fn all() -> Self {
        Self((Layer::L1 as u8) | (Layer::L2 as u8) | (Layer::L3 as u8))
    }

    /// Insert `layer` into the set. L1 cannot be removed.
    pub const fn insert(&mut self, layer: Layer) {
        self.0 |= layer as u8;
    }

    /// Test whether `layer` is in the set.
    #[must_use]
    pub const fn contains(self, layer: Layer) -> bool {
        self.0 & (layer as u8) != 0
    }

    /// Raw wire byte. The encoder writes this directly; the decoder
    /// passes the byte to [`Self::from_wire`]. L1 is always forced on
    /// so peers can rely on the invariant.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0 | (Layer::L1 as u8)
    }

    /// Inverse of [`Self::as_wire`]. Unknown bits beyond L1/L2/L3 are
    /// silently dropped (forward-compat per Appendix A) but L1 is
    /// always forced on.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Self {
        let known = (Layer::L1 as u8) | (Layer::L2 as u8) | (Layer::L3 as u8);
        Self((byte & known) | (Layer::L1 as u8))
    }
}

impl Default for LayerSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Wire bit advertising acknowledged, idempotent input batches.
pub const ACKNOWLEDGED_INPUT: u32 = 0x0000_0010;
/// Wire bit advertising chunked, acknowledged `Command::PutFile` uploads.
pub const FILE_UPLOAD: u32 = 0x0000_0020;

/// An additive server-owned protocol feature.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServerFeature {
    /// The server accepts idempotent `Command::ApplyInput` batches.
    AcknowledgedInput = ACKNOWLEDGED_INPUT,
    /// The server accepts sandboxed, chunked `Command::PutFile` uploads.
    FileUpload = FILE_UPLOAD,
}

/// Bit-field of additive server-owned protocol features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ServerFeatureSet(u32);

impl ServerFeatureSet {
    const KNOWN: u32 =
        (ServerFeature::AcknowledgedInput as u32) | (ServerFeature::FileUpload as u32);

    /// Empty set for servers that advertise no additive features.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Build a set containing all listed features.
    #[must_use]
    pub const fn with(features: &[ServerFeature]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < features.len() {
            bits |= features[i] as u32;
            i += 1;
        }
        Self(bits)
    }

    /// Test whether `feature` is advertised.
    #[must_use]
    pub const fn contains(self, feature: ServerFeature) -> bool {
        self.0 & (feature as u32) != 0
    }

    /// True when no feature bits are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Raw wire bits, with unknown bits excluded.
    #[must_use]
    pub const fn as_wire(self) -> u32 {
        self.0 & Self::KNOWN
    }

    /// Decode known feature bits while ignoring future unknown bits.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }
}

/// One image-transport protocol the client may advertise (SPEC §6.2).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageProtocol {
    /// VT340 sixel graphics, transported via DCS.
    Sixel = 1 << 0,
    /// Kitty graphics protocol, transported via APC `G` payloads.
    KittyGraphics = 1 << 1,
    /// iTerm2 inline images, transported via OSC 1337.
    Iterm2 = 1 << 2,
}

/// A bit-field of [`ImageProtocol`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageProtocolSet(u8);

impl ImageProtocolSet {
    const KNOWN: u8 = (ImageProtocol::Sixel as u8)
        | (ImageProtocol::KittyGraphics as u8)
        | (ImageProtocol::Iterm2 as u8);

    /// Empty set: no image protocols supported.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// All currently-defined image protocols.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Build a set containing all listed protocols.
    #[must_use]
    pub const fn with(protocols: &[ImageProtocol]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < protocols.len() {
            bits |= protocols[i] as u8;
            i += 1;
        }
        Self(bits)
    }

    /// Test whether `protocol` is in the set.
    #[must_use]
    pub const fn contains(self, protocol: ImageProtocol) -> bool {
        self.0 & (protocol as u8) != 0
    }

    /// Raw wire byte.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0 & Self::KNOWN
    }

    /// Inverse of [`Self::as_wire`]. Unknown bits are ignored.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Self {
        Self(byte & Self::KNOWN)
    }
}

impl Default for ImageProtocolSet {
    fn default() -> Self {
        Self::all()
    }
}

/// One keyboard protocol the client may advertise (SPEC §6.2).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyboardProtocol {
    /// Kitty keyboard protocol APC replies.
    Kitty = 1 << 0,
    /// xterm modifyOtherKeys-style replies.
    ModifyOtherKeys = 1 << 1,
}

/// A bit-field of [`KeyboardProtocol`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyboardProtocolSet(u8);

impl KeyboardProtocolSet {
    const KNOWN: u8 = (KeyboardProtocol::Kitty as u8) | (KeyboardProtocol::ModifyOtherKeys as u8);

    /// Empty set: no keyboard extension protocols supported.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// All currently-defined keyboard protocols.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Build a set containing all listed protocols.
    #[must_use]
    pub const fn with(protocols: &[KeyboardProtocol]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < protocols.len() {
            bits |= protocols[i] as u8;
            i += 1;
        }
        Self(bits)
    }

    /// Test whether `protocol` is in the set.
    #[must_use]
    pub const fn contains(self, protocol: KeyboardProtocol) -> bool {
        self.0 & (protocol as u8) != 0
    }

    /// True when any keyboard protocol is advertised.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Raw wire byte.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0 & Self::KNOWN
    }

    /// Inverse of [`Self::as_wire`]. Unknown bits are ignored.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Self {
        Self(byte & Self::KNOWN)
    }
}

impl Default for KeyboardProtocolSet {
    fn default() -> Self {
        Self::all()
    }
}

/// The client's advertised capability set, per SPEC §6.2.
///
/// SPEC §6.2 enumerates `kbd_protocols`, `mouse_protocols`, `color`,
/// `images`, `hyperlinks`, `unicode_version`, the deprecated `rendering`
/// mode, and the `layers` bitset. This struct carries the fields currently
/// wired into HELLO; sibling tickets add the remaining fields behind their
/// own wire bumps. The struct is `#[non_exhaustive]` so additive fields don't
/// break downstream literal construction.
///
/// Construct via [`Self::new`] (defaults across the board) plus the
/// builder setters; that's the path that survives field-set growth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ClientCapabilities {
    /// The client's color tier (SPEC §6.2). See [`ColorSupport`].
    pub color_support: ColorSupport,
    /// The set of conformance tiers (SPEC §6.2 / §16) the client speaks.
    /// L1 is always implied; clients add L2 / L3 to opt in to the
    /// respective optional services. The reference TUI advertises
    /// [`LayerSet::all`]; an agent / recorder advertises [`LayerSet::new`]
    /// (L1-only).
    pub layers: LayerSet,
    /// Image protocols the client can render (SPEC §6.2).
    pub image_protocols: ImageProtocolSet,
    /// Keyboard extension protocols the client understands (SPEC §6.2).
    pub kbd_protocols: KeyboardProtocolSet,
    /// Whether OSC 8 hyperlink framing may be forwarded to the client.
    pub hyperlinks: bool,
    /// Which server emitter this consumer wants for terminal content
    /// (SPEC §6.2). Defaults to [`OutputMode::Raw`] — the byte-faithful
    /// human-TUI path; agent / state-sync consumers opt into
    /// [`OutputMode::StateSync`] (phux-fseo).
    pub output_mode: OutputMode,
    /// The outer terminal's effective default foreground/background colors.
    ///
    /// Interactive clients probe OSC 10/11 before entering raw mode and
    /// advertise the result here. The server installs these defaults on its
    /// terminal emulator so programs inside phux receive the same OSC query
    /// replies they receive when run directly in the host terminal. `None`
    /// is the compatibility value for non-TTY and older clients.
    pub default_colors: Option<TerminalDefaultColors>,
}

/// Effective default colors reported by the client's outer terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalDefaultColors {
    /// Effective default foreground (OSC 10).
    pub foreground: TerminalColor,
    /// Effective default background (OSC 11).
    pub background: TerminalColor,
}

/// A 24-bit RGB color carried in terminal capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TerminalColor {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
}

impl ClientCapabilities {
    /// Build a default capability set: `ColorSupport::TrueColor` plus the
    /// L1-only layer set. Call sites that want to override one field call
    /// the matching `.with_*` setter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            color_support: ColorSupport::TrueColor,
            layers: LayerSet::new(),
            image_protocols: ImageProtocolSet::all(),
            kbd_protocols: KeyboardProtocolSet::all(),
            hyperlinks: true,
            output_mode: OutputMode::Raw,
            default_colors: None,
        }
    }

    /// Builder setter for [`Self::output_mode`].
    #[must_use]
    pub const fn with_output_mode(mut self, output_mode: OutputMode) -> Self {
        self.output_mode = output_mode;
        self
    }

    /// Builder setter for [`Self::default_colors`].
    #[must_use]
    pub const fn with_default_colors(mut self, colors: TerminalDefaultColors) -> Self {
        self.default_colors = Some(colors);
        self
    }

    /// Builder setter for [`Self::color_support`].
    #[must_use]
    pub const fn with_color_support(mut self, color_support: ColorSupport) -> Self {
        self.color_support = color_support;
        self
    }

    /// Builder setter for [`Self::layers`].
    #[must_use]
    pub const fn with_layers(mut self, layers: LayerSet) -> Self {
        self.layers = layers;
        self
    }

    /// Builder setter for [`Self::image_protocols`].
    #[must_use]
    pub const fn with_image_protocols(mut self, image_protocols: ImageProtocolSet) -> Self {
        self.image_protocols = image_protocols;
        self
    }

    /// Builder setter for [`Self::kbd_protocols`].
    #[must_use]
    pub const fn with_kbd_protocols(mut self, kbd_protocols: KeyboardProtocolSet) -> Self {
        self.kbd_protocols = kbd_protocols;
        self
    }

    /// Builder setter for [`Self::hyperlinks`].
    #[must_use]
    pub const fn with_hyperlinks(mut self, hyperlinks: bool) -> Self {
        self.hyperlinks = hyperlinks;
        self
    }
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// What the server advertises back in `HELLO_OK` (SPEC §6.1).
///
/// The client declares what it *wants* via [`ClientCapabilities`]; the
/// server declares what it *implements* here. The negotiated conformance
/// tier set is the intersection of the two `layers` bit-fields
/// ([ADR-0015](../../ADR/0015-protocol-layering.md) §"Conformance tiers").
/// L1 is always implemented and always present on the wire.
///
/// This is deliberately narrow today — `layers` is the only negotiated
/// axis the server owns. Color / image / keyboard tiers are client-render
/// concerns carried by [`ClientCapabilities`], so they have no server-side
/// counterpart. Future server-owned capabilities append as additive
/// trailing fields (the encoding grows monotonically, same discipline as
/// [`ClientCapabilities`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCapabilities {
    /// The conformance tiers (SPEC §6.2 / §16) the server mounts. L1 is
    /// always implemented; the server adds L2 / L3 when those services
    /// are wired. See [`LayerSet`].
    pub layers: LayerSet,
    /// Additive server-owned protocol features.
    pub features: ServerFeatureSet,
}

impl ServerCapabilities {
    /// Build a default server capability set: L1 only. Call [`Self::with_layers`]
    /// to advertise the higher tiers the server actually mounts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            layers: LayerSet::new(),
            features: ServerFeatureSet::new(),
        }
    }

    /// Builder setter for [`Self::layers`].
    #[must_use]
    pub const fn with_layers(mut self, layers: LayerSet) -> Self {
        self.layers = layers;
        self
    }

    /// Builder setter for [`Self::features`].
    #[must_use]
    pub const fn with_features(mut self, features: ServerFeatureSet) -> Self {
        self.features = features;
        self
    }
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the client terminal's color tier from environment hints.
///
/// The heuristic mirrors what well-known TUIs (tmux, neovim, htop) use:
///
/// 1. **`$COLORTERM`** is the canonical signal — values `truecolor` and
///    `24bit` mean direct RGB is safe.
/// 2. **`$TERM`** suffixes (`*-256color`, `*-direct`, `*-truecolor`) carry
///    the next-most-reliable signal.
/// 3. **`$TERM_PROGRAM`** covers macOS Terminal.app / iTerm.app where
///    `$COLORTERM` is often unset.
/// 4. Fallback: [`ColorSupport::TrueColor`] (most-permissive). The server
///    downsamples on the way out; an over-claim is recoverable. An
///    under-claim would silently degrade output even on capable terminals,
///    so we err generous.
///
/// This intentionally never returns [`ColorSupport::Mono`] — that tier is
/// reserved for explicit opt-in (config flag, accessibility profile) and
/// is not a signal any environment variable carries reliably.
#[must_use]
pub fn detect_color_support() -> ColorSupport {
    detect_from_env(|key| std::env::var(key).ok())
}

/// Pure (testable) form of [`detect_color_support`]: takes a lookup
/// closure so tests can simulate arbitrary environments without
/// `unsafe { std::env::set_var }`.
fn detect_from_env<F>(env: F) -> ColorSupport
where
    F: Fn(&str) -> Option<String>,
{
    // 1. $COLORTERM — the most authoritative signal.
    if let Some(ct) = env("COLORTERM") {
        let ct_lc = ct.to_ascii_lowercase();
        if ct_lc == "truecolor" || ct_lc == "24bit" {
            return ColorSupport::TrueColor;
        }
    }

    // 2. $TERM suffix.
    let term = env("TERM").unwrap_or_default();
    let term_lc = term.to_ascii_lowercase();
    if term_lc.ends_with("-direct") || term_lc.ends_with("-truecolor") {
        return ColorSupport::TrueColor;
    }
    if term_lc.ends_with("-256color") {
        return ColorSupport::Indexed256;
    }
    if !term_lc.is_empty() && !term_lc.contains("color") {
        // `xterm`, `linux`, `vt100`, etc. — assume 16-color baseline.
        // Anything richer would have advertised a `-256color` or
        // `-direct` suffix.
        // Common exception: macOS Terminal.app sets `TERM=xterm-256color`
        // so this branch only catches the genuine vt100/linux/etc cases.
        if term_lc == "dumb" {
            return ColorSupport::Mono;
        }
        return ColorSupport::Indexed16;
    }

    // 3. $TERM_PROGRAM — macOS native terminals.
    if let Some(tp) = env("TERM_PROGRAM") {
        let tp_lc = tp.to_ascii_lowercase();
        // iTerm.app and WezTerm advertise truecolor; Apple_Terminal
        // (macOS Terminal.app) is 256-color only.
        if tp_lc == "iterm.app" || tp_lc == "wezterm" {
            return ColorSupport::TrueColor;
        }
        if tp_lc == "apple_terminal" {
            return ColorSupport::Indexed256;
        }
    }

    // 4. Fallback: assume the user is on a modern truecolor terminal that
    // forgot to advertise. Over-claiming is recoverable (server downsamples
    // anyway if a later signal arrives); under-claiming silently degrades.
    ColorSupport::TrueColor
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn color_support_wire_roundtrips_every_variant() {
        for v in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Indexed16,
            ColorSupport::Mono,
        ] {
            let tag = v.as_wire();
            let back = ColorSupport::from_wire(tag).expect("known tag");
            assert_eq!(back, v);
        }
    }

    #[test]
    fn unknown_color_support_tag_is_none() {
        assert!(ColorSupport::from_wire(0xFF).is_none());
    }

    #[test]
    fn image_protocol_set_ignores_unknown_bits() {
        let set = ImageProtocolSet::from_wire(0xFF);
        assert!(set.contains(ImageProtocol::Sixel));
        assert!(set.contains(ImageProtocol::KittyGraphics));
        assert!(set.contains(ImageProtocol::Iterm2));
        assert_eq!(set.as_wire(), ImageProtocolSet::all().as_wire());
    }

    #[test]
    fn keyboard_protocol_set_ignores_unknown_bits() {
        let set = KeyboardProtocolSet::from_wire(0xFF);
        assert!(set.contains(KeyboardProtocol::Kitty));
        assert!(set.contains(KeyboardProtocol::ModifyOtherKeys));
        assert_eq!(set.as_wire(), KeyboardProtocolSet::all().as_wire());
    }

    #[test]
    fn colorterm_truecolor_wins() {
        let env = env_map(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]);
        assert_eq!(detect_from_env(env), ColorSupport::TrueColor);
    }

    #[test]
    fn colorterm_24bit_wins() {
        let env = env_map(&[("COLORTERM", "24bit"), ("TERM", "xterm")]);
        assert_eq!(detect_from_env(env), ColorSupport::TrueColor);
    }

    #[test]
    fn term_256color_maps_to_indexed256() {
        let env = env_map(&[("TERM", "xterm-256color")]);
        assert_eq!(detect_from_env(env), ColorSupport::Indexed256);
    }

    #[test]
    fn term_direct_maps_to_truecolor() {
        let env = env_map(&[("TERM", "xterm-direct")]);
        assert_eq!(detect_from_env(env), ColorSupport::TrueColor);
    }

    #[test]
    fn term_xterm_maps_to_indexed16() {
        let env = env_map(&[("TERM", "xterm")]);
        assert_eq!(detect_from_env(env), ColorSupport::Indexed16);
    }

    #[test]
    fn term_dumb_maps_to_mono() {
        let env = env_map(&[("TERM", "dumb")]);
        assert_eq!(detect_from_env(env), ColorSupport::Mono);
    }

    #[test]
    fn macos_terminal_falls_back_to_indexed256() {
        let env = env_map(&[("TERM_PROGRAM", "Apple_Terminal")]);
        assert_eq!(detect_from_env(env), ColorSupport::Indexed256);
    }

    #[test]
    fn iterm_advertises_truecolor() {
        let env = env_map(&[("TERM_PROGRAM", "iTerm.app")]);
        assert_eq!(detect_from_env(env), ColorSupport::TrueColor);
    }

    #[test]
    fn unknown_env_falls_back_to_truecolor() {
        let env = env_map(&[]);
        assert_eq!(detect_from_env(env), ColorSupport::TrueColor);
    }

    #[test]
    fn client_capabilities_default_is_truecolor() {
        let caps = ClientCapabilities::default();
        assert_eq!(caps.color_support, ColorSupport::TrueColor);
        assert!(caps.image_protocols.contains(ImageProtocol::Sixel));
        assert!(caps.kbd_protocols.contains(KeyboardProtocol::Kitty));
        assert!(caps.hyperlinks);
    }

    #[test]
    fn client_capabilities_builder() {
        let caps = ClientCapabilities::new().with_color_support(ColorSupport::Indexed16);
        assert_eq!(caps.color_support, ColorSupport::Indexed16);
    }

    #[test]
    fn server_feature_bits_are_stable() {
        assert_eq!(ACKNOWLEDGED_INPUT, 0x0000_0010);
        assert_eq!(FILE_UPLOAD, 0x0000_0020);
        assert_eq!(ServerFeature::AcknowledgedInput as u32, ACKNOWLEDGED_INPUT);
        assert_eq!(ServerFeature::FileUpload as u32, FILE_UPLOAD);
        let set =
            ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput, ServerFeature::FileUpload]);
        assert!(set.contains(ServerFeature::AcknowledgedInput));
        assert!(set.contains(ServerFeature::FileUpload));
        assert_eq!(set.as_wire(), 0x0000_0030);
        assert_eq!(ServerFeatureSet::from_wire(u32::MAX), set);
    }
}
