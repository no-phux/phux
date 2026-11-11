//! The bootstrap codec, profile, and stream-profile sub-record codecs.
//!
//! Only a server sends these. A Terminal profile or engine codec names a
//! choice from the client's own offer, so one the client does not know is
//! malformed, not newer: skipping `HELLO_OK` would hang the handshake. Every
//! other kind's codec is fixed by the kind and never offered, so a codec tag
//! the client does not know is another kind's newer codec: it decodes as
//! `Unknown` and the client refuses only that resource.

use crate::caps::{
    BootstrapCodec, BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

pub(in crate::wire) fn encode_bootstrap_codec(codec: BootstrapCodec, enc: &mut Encoder<'_>) {
    debug_assert!(
        match codec {
            BootstrapCodec::Unknown { tag } => !BootstrapCodec::is_allocated_tag(tag),
            BootstrapCodec::Kind { tag } => {
                tag != BootstrapCodec::NATIVE_TAG
                    && matches!(
                        BootstrapCodec::from_tag_in(&[], tag),
                        BootstrapCodec::Unknown { .. }
                    )
            }
            _ => true,
        },
        "{codec:?} carries a tag another variant names"
    );
    enc.write_u8(codec.tag());
    if let BootstrapCodec::Native(version) = codec {
        enc.write_u8(version.as_wire());
    }
}

pub(in crate::wire) fn decode_bootstrap_codec(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapCodec, DecodeError> {
    match dec.read_u8()? {
        BootstrapCodec::NATIVE_TAG => {
            let value = dec.read_u8()?;
            let codec =
                EngineCodec::from_wire(value).ok_or(DecodeError::InvalidBootstrapProfile)?;
            Ok(BootstrapCodec::Native(codec))
        }
        tag => Ok(BootstrapCodec::from_tag(tag)),
    }
}

pub(in crate::wire) fn encode_bootstrap_profile(profile: BootstrapProfile, enc: &mut Encoder<'_>) {
    match profile {
        BootstrapProfile::NativeState { codec, features } => {
            enc.write_u8(BootstrapProfile::NATIVE_STATE_TAG);
            enc.write_u8(codec.as_wire());
            enc.write_u32_be(features.as_wire());
        }
        BootstrapProfile::SynthesizedVtRaw => {
            enc.write_u8(BootstrapProfile::SYNTHESIZED_VT_RAW_TAG);
        }
        BootstrapProfile::SynthesizedVtStateSync => {
            enc.write_u8(BootstrapProfile::SYNTHESIZED_VT_STATE_SYNC_TAG);
        }
    }
}

pub(in crate::wire) fn decode_bootstrap_profile(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapProfile, DecodeError> {
    match dec.read_u8()? {
        BootstrapProfile::NATIVE_STATE_TAG => {
            let value = dec.read_u8()?;
            let codec =
                EngineCodec::from_wire(value).ok_or(DecodeError::InvalidBootstrapProfile)?;
            let features = EngineFeatureSet::from_wire(dec.read_u32_be()?);
            if !features.supports_native() {
                return Err(DecodeError::InvalidBootstrapProfile);
            }
            Ok(BootstrapProfile::NativeState { codec, features })
        }
        BootstrapProfile::SYNTHESIZED_VT_RAW_TAG => Ok(BootstrapProfile::SynthesizedVtRaw),
        BootstrapProfile::SYNTHESIZED_VT_STATE_SYNC_TAG => {
            Ok(BootstrapProfile::SynthesizedVtStateSync)
        }
        _ => Err(DecodeError::InvalidBootstrapProfile),
    }
}

/// The stream `BOOTSTRAP_BEGIN` describes: a Terminal profile, or the
/// kind-fixed codec of the catalog kind that streams it, raw, with a cursor
/// only where that kind's contract resumes.
pub(in crate::wire) const fn decode_bootstrap_stream_profile(
    codec: BootstrapCodec,
    output_mode: u8,
    resume_after_seq: Option<u64>,
) -> Result<BootstrapStreamProfile, DecodeError> {
    match (codec, output_mode, resume_after_seq) {
        (BootstrapCodec::Native(codec), 0, None) => {
            Ok(BootstrapStreamProfile::NativeState { codec })
        }
        (BootstrapCodec::SynthesizedVtV1, 0, None) => Ok(BootstrapStreamProfile::SynthesizedVtRaw),
        (BootstrapCodec::SynthesizedVtV1, 1, None) => {
            Ok(BootstrapStreamProfile::SynthesizedVtStateSync)
        }
        // A newer codec's output mode and cursor mean what it says they do.
        (BootstrapCodec::Unknown { tag }, _, _) => {
            Ok(BootstrapStreamProfile::Unknown { codec_tag: tag })
        }
        (codec, 0, after_seq) => {
            match BootstrapStreamProfile::for_kind_codec_in(&crate::kinds::KINDS, codec, after_seq)
            {
                Some(profile) => Ok(profile),
                None => Err(DecodeError::InvalidBootstrapProfile),
            }
        }
        _ => Err(DecodeError::InvalidBootstrapProfile),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::OutputMode;

    /// Each stream profile's `BOOTSTRAP_BEGIN` codec bytes, output mode and
    /// resume cursor, pinned: the catalog decides which kind owns a tag,
    /// never the bytes.
    #[test]
    fn every_stream_profile_encodes_its_pinned_codec_bytes() {
        use BootstrapStreamProfile as P;
        let v2 = EngineCodec::LibghosttyCheckpointV2;
        let pre1 = EngineCodec::LibghosttyPre1SnapshotV1;
        let snapshot = EngineCodec::LibghosttySnapshotV1;
        let raw = OutputMode::Raw;
        let cases: [(P, &[u8], OutputMode, Option<u64>); 11] = [
            (P::NativeState { codec: v2 }, &[1, 2], raw, None),
            (P::NativeState { codec: pre1 }, &[1, 4], raw, None),
            (P::NativeState { codec: snapshot }, &[1, 5], raw, None),
            (P::SynthesizedVtRaw, &[0], raw, None),
            (P::SynthesizedVtStateSync, &[0], OutputMode::StateSync, None),
            (P::AgentEventsJsonlV1, &[3], raw, None),
            (
                P::AgentEventsJsonlResumeV1 { after_seq: 7 },
                &[3],
                raw,
                Some(7),
            ),
            (P::AccessibilityTreeJsonV1, &[4], raw, None),
            (P::PixelSurfacePngV1, &[5], raw, None),
            (P::DocumentTypedV1, &[6], raw, None),
            (P::Unknown { codec_tag: 200 }, &[200], raw, None),
        ];
        for (profile, bytes, mode, resume) in cases {
            let mut buf = bytes::BytesMut::new();
            encode_bootstrap_codec(profile.codec(), &mut Encoder::new(&mut buf));
            assert_eq!(&buf[..], bytes, "{profile:?}");
            assert_eq!(profile.output_mode(), mode, "{profile:?}");
            assert_eq!(profile.resume_after_seq(), resume, "{profile:?}");
            let codec = decode_bootstrap_codec(&mut Decoder::new(&buf)).unwrap();
            assert_eq!(
                decode_bootstrap_stream_profile(codec, mode.as_wire(), resume),
                Ok(profile)
            );
        }
    }
}
