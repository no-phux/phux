//! The `HELLO` / `HELLO_OK` capability sets and the conformance tiers
//! they carry.

use super::{
    BootstrapCapabilities, ColorSupport, Compression, CompressionSet, OutputMode,
    ServerFeatureExtSet, ServerFeatureSet,
};

// -----------------------------------------------------------------------------
// Layer / LayerSet — SPEC §6.2 conformance-tier bitset (ADR-0015).
// -----------------------------------------------------------------------------

/// A single conformance tier (SPEC §6.2 / §16).
///
/// L1 is always implied; the negotiated set is the intersection of both peers'
/// [`LayerSet`]s, and out-of-tier messages MUST surface as protocol errors
/// (§16.4, ADR-0015).
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

/// A bit-field of [`Layer`]s, one `u8` on the wire. L1 is always set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerSet(u8);

impl LayerSet {
    /// The L1-only set.
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

    /// The full set: L1 + L2 + L3.
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

    /// Raw wire byte, L1 forced on.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        self.0 | (Layer::L1 as u8)
    }

    /// Inverse of [`Self::as_wire`]: unknown bits dropped, L1 forced on.
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

u8_flag_set! {
    /// A bit-field of [`ImageProtocol`]s.
    ImageProtocolSet of ImageProtocol { Sixel, KittyGraphics, Iterm2 }
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

u8_flag_set! {
    /// A bit-field of [`KeyboardProtocol`]s.
    KeyboardProtocolSet of KeyboardProtocol { Kitty, ModifyOtherKeys }
}

impl KeyboardProtocolSet {
    /// True when no keyboard protocol is advertised.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// The client's advertised capability set (SPEC §6.2). Construct with
/// [`Self::new`] and the `with_*` setters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ClientCapabilities {
    /// The client's color tier.
    pub color_support: ColorSupport,
    /// The conformance tiers the client speaks (SPEC §6.2 / §16).
    pub layers: LayerSet,
    /// Image protocols the client can render.
    pub image_protocols: ImageProtocolSet,
    /// Keyboard extension protocols the client understands.
    pub kbd_protocols: KeyboardProtocolSet,
    /// Whether OSC 8 hyperlink framing may be forwarded to the client.
    pub hyperlinks: bool,
    /// Preferred compatibility emitter when native is not selected.
    pub output_mode: OutputMode,
    /// The outer terminal's default colors (OSC 10/11), installed on the
    /// server's emulator so OSC queries inside phux answer as they would
    /// outside. `None` for non-TTY and older clients.
    pub default_colors: Option<TerminalDefaultColors>,
    /// Explicit bootstrap profiles, exact native codecs/features, and receive bounds.
    pub bootstrap: BootstrapCapabilities,
    /// Frame compressions this client can inflate (`docs/spec/proto.md`
    /// §6.4). Empty is right for a Unix socket.
    pub compression: CompressionSet,
    /// The ssh endpoints `phux stdio-bridge` stamps on the HELLO it relays
    /// (HELLO field 9, `docs/spec/L3.md` §3.9).
    pub ssh_origin: Option<crate::wire::ssh_origin::SshOrigin>,
    /// Whether this client can demultiplex per-Terminal QUIC streams. An
    /// explicit offer, never inferred from the transport.
    pub quic_streams: bool,
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
    /// The default capability set: truecolor, L1 only, raw output.
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
            bootstrap: BootstrapCapabilities::new(),
            compression: CompressionSet::new(),
            ssh_origin: None,
            quic_streams: false,
        }
    }

    /// Builder setter for [`Self::compression`].
    #[must_use]
    pub const fn with_compression(mut self, compression: CompressionSet) -> Self {
        self.compression = compression;
        self
    }

    /// Builder setter for [`Self::ssh_origin`].
    #[must_use]
    pub const fn with_ssh_origin(mut self, origin: crate::wire::ssh_origin::SshOrigin) -> Self {
        self.ssh_origin = Some(origin);
        self
    }

    /// Builder setter for [`Self::quic_streams`].
    #[must_use]
    pub const fn with_quic_streams(mut self, enabled: bool) -> Self {
        self.quic_streams = enabled;
        self
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
    /// Builder setter for [`Self::bootstrap`].
    #[must_use]
    pub const fn with_bootstrap(mut self, bootstrap: BootstrapCapabilities) -> Self {
        self.bootstrap = bootstrap;
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

/// What the server implements, advertised in `HELLO_OK` (SPEC §6.1). New
/// server-owned capabilities append as trailing fields; the next is a second
/// feature word (ADR-0137).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerCapabilities {
    /// The conformance tiers the server mounts (SPEC §6.2 / §16).
    pub layers: LayerSet,
    /// Additive server-owned protocol features.
    pub features: ServerFeatureSet,
    /// Additive features in the second trailing `u32` of the caps record.
    pub features_ext: ServerFeatureExtSet,
    /// The frame compression this server selected for the connection
    /// (`docs/spec/proto.md` §6.4). Always one the client offered.
    pub compression: Compression,
}

impl ServerCapabilities {
    /// The default server capability set: L1 only, no features.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            layers: LayerSet::new(),
            features: ServerFeatureSet::new(),
            features_ext: ServerFeatureExtSet::new(),
            compression: Compression::None,
        }
    }

    /// Builder setter for [`Self::compression`].
    #[must_use]
    pub const fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
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

    /// Builder setter for [`Self::features_ext`].
    #[must_use]
    pub const fn with_features_ext(mut self, features: ServerFeatureExtSet) -> Self {
        self.features_ext = features;
        self
    }
}

impl Default for ServerCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_sets_ignore_unknown_bits() {
        assert_eq!(ImageProtocolSet::from_wire(0xFF), ImageProtocolSet::all());
        assert_eq!(ImageProtocolSet::all().as_wire(), 0x07);
        assert_eq!(
            KeyboardProtocolSet::from_wire(0xFF),
            KeyboardProtocolSet::all()
        );
        assert_eq!(KeyboardProtocolSet::all().as_wire(), 0x03);
    }
}
