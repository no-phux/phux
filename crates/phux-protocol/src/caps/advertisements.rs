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

/// Detect the client terminal's color tier from environment hints.
///
/// `$COLORTERM`, then `$TERM` (suffixes; `dumb` is mono), then
/// `$TERM_PROGRAM`, falling back to truecolor: an over-claim is recoverable,
/// an under-claim silently degrades.
#[must_use]
pub fn detect_color_support() -> ColorSupport {
    detect_from_env(|key| std::env::var(key).ok())
}

/// [`detect_color_support`] over an injected environment lookup.
fn detect_from_env<F>(env: F) -> ColorSupport
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(ct) = env("COLORTERM") {
        let ct_lc = ct.to_ascii_lowercase();
        if ct_lc == "truecolor" || ct_lc == "24bit" {
            return ColorSupport::TrueColor;
        }
    }

    let term = env("TERM").unwrap_or_default();
    let term_lc = term.to_ascii_lowercase();
    if term_lc.ends_with("-direct") || term_lc.ends_with("-truecolor") {
        return ColorSupport::TrueColor;
    }
    if term_lc.ends_with("-256color") {
        return ColorSupport::Indexed256;
    }
    if !term_lc.is_empty() && !term_lc.contains("color") {
        // `xterm`, `linux`, `vt100`: anything richer carries a suffix.
        if term_lc == "dumb" {
            return ColorSupport::Mono;
        }
        return ColorSupport::Indexed16;
    }

    if let Some(tp) = env("TERM_PROGRAM") {
        let tp_lc = tp.to_ascii_lowercase();
        if tp_lc == "iterm.app" || tp_lc == "wezterm" {
            return ColorSupport::TrueColor;
        }
        if tp_lc == "apple_terminal" {
            return ColorSupport::Indexed256;
        }
    }

    ColorSupport::TrueColor
}
