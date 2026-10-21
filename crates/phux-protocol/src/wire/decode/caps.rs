//! The positional client and server capability records of `HELLO` and `HELLO_OK`.

use super::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfileSet, DecodeError, Decoder,
    EngineCodecSet, EngineFeatureSet, req,
};

/// Decode a `HELLO` frame's positional `client_caps` field value.
///
/// `HELLO` precedes negotiation, so a value this build does not know cannot
/// be refused in place: it would close the connection and a newer client
/// would redial forever. Every byte is therefore total. An unknown
/// `ColorSupport` is `TrueColor` and an unknown `OutputMode` is `Raw` (the
/// defaults), a flag or presence byte is set when nonzero, and an
/// over-large payload limit is clamped to this build's cap. Bytes appended
/// after the bootstrap block are skipped.
pub(super) fn decode_client_capabilities(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ClientCapabilities, DecodeError> {
    let color_support = decode_color_support(d)?;
    let layers = crate::caps::LayerSet::from_wire(d.read_u8()?);
    let images = crate::caps::ImageProtocolSet::from_wire(d.read_u8()?);
    let keyboards = crate::caps::KeyboardProtocolSet::from_wire(d.read_u8()?);
    let hyperlinks = decode_hyperlinks_flag(d)?;
    let output_mode = decode_output_mode(d)?;
    let default_colors = decode_default_colors(d)?;
    let bootstrap = decode_bootstrap_capabilities(d)?;
    let mut caps = crate::caps::ClientCapabilities::new()
        .with_color_support(color_support)
        .with_layers(layers)
        .with_image_protocols(images)
        .with_kbd_protocols(keyboards)
        .with_hyperlinks(hyperlinks)
        .with_output_mode(output_mode)
        .with_bootstrap(bootstrap);
    if let Some(colors) = default_colors {
        caps = caps.with_default_colors(colors);
    }
    Ok(caps)
}

/// Decode the color-support byte of a client capability block; an unknown
/// tier is `TrueColor`, so an unknown client is never silently downgraded.
pub(super) fn decode_color_support(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ColorSupport, DecodeError> {
    Ok(crate::caps::ColorSupport::from_wire(d.read_u8()?).unwrap_or_default())
}

/// Decode the hyperlink-support flag of a client capability block: any
/// nonzero byte is set.
pub(super) fn decode_hyperlinks_flag(d: &mut Decoder<'_>) -> Result<bool, DecodeError> {
    Ok(d.read_u8()? != 0)
}

/// Decode the output-mode byte of a client capability block; an unknown
/// mode is `Raw`.
pub(super) fn decode_output_mode(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::OutputMode, DecodeError> {
    Ok(crate::caps::OutputMode::from_wire(d.read_u8()?))
}

/// Decode the presence-tagged default palette of a client capability block.
///
/// The presence byte is a flag: `0` is absent and any other value means the
/// six foreground and background bytes follow. A richer palette therefore
/// keeps those six bytes and appends the rest after the bootstrap block,
/// where this build skips it.
pub(super) fn decode_default_colors(
    d: &mut Decoder<'_>,
) -> Result<Option<crate::caps::TerminalDefaultColors>, DecodeError> {
    let palette_tag = d.read_u8()?;
    match palette_tag {
        0 => Ok(None),
        _ => Ok(Some(crate::caps::TerminalDefaultColors {
            foreground: crate::caps::TerminalColor {
                r: d.read_u8()?,
                g: d.read_u8()?,
                b: d.read_u8()?,
            },
            background: crate::caps::TerminalColor {
                r: d.read_u8()?,
                g: d.read_u8()?,
                b: d.read_u8()?,
            },
        })),
    }
}

/// Decode the bootstrap capability block of a client capability field.
pub(super) fn decode_bootstrap_capabilities(
    d: &mut Decoder<'_>,
) -> Result<BootstrapCapabilities, DecodeError> {
    let profiles = BootstrapProfileSet::from_wire(d.read_u8()?);
    let native_codecs = EngineCodecSet::from_wire(d.read_u64_be()?);
    let native_features = EngineFeatureSet::from_wire(d.read_u32_be()?);
    let max_chunk_bytes = d.read_u32_be()?;
    let max_history_page_bytes = d.read_u32_be()?;
    let limits = BootstrapLimits::clamped(max_chunk_bytes, max_history_page_bytes);
    Ok(BootstrapCapabilities {
        profiles,
        native_codecs,
        native_features,
        limits,
    })
}

/// Decode a `HELLO_OK` frame's `server_caps` field value.
///
/// The feature word is trailing-optional, and bytes after it are ignored:
/// ADR-0137 reserves them for `features_ext`.
pub(super) fn decode_server_capabilities(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ServerCapabilities, DecodeError> {
    let mut caps = crate::caps::ServerCapabilities::new()
        .with_layers(crate::caps::LayerSet::from_wire(d.read_u8()?));
    if !d.at_body_end() {
        caps = caps.with_features(crate::caps::ServerFeatureSet::from_wire(d.read_u32_be()?));
    }
    if !d.at_body_end() {
        caps = caps.with_features_ext(crate::caps::ServerFeatureExtSet::from_wire(
            d.read_u32_be()?,
        ));
    }
    Ok(caps)
}

/// Rebuild the two required payload limits a `HELLO_OK` negotiated.
pub(super) fn negotiated_bootstrap_limits(
    max_chunk_bytes: Option<u32>,
    max_history_page_bytes: Option<u32>,
) -> Result<BootstrapLimits, DecodeError> {
    BootstrapLimits::new(req(max_chunk_bytes)?, req(max_history_page_bytes)?)
        .ok_or(DecodeError::BootstrapLimitExceeded)
}
