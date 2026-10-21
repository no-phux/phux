//! The bootstrap codec, profile, and stream-profile sub-record codecs.

use crate::caps::{
    BootstrapCodec, BootstrapProfile, BootstrapStreamProfile, EngineCodec, EngineFeatureSet,
};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;

pub(in crate::wire) fn encode_bootstrap_codec(codec: BootstrapCodec, enc: &mut Encoder<'_>) {
    match codec {
        BootstrapCodec::SynthesizedVtV1 => {
            enc.write_u8(BootstrapCodec::SYNTHESIZED_VT_V1_TAG);
        }
        BootstrapCodec::Native(version) => {
            enc.write_u8(BootstrapCodec::NATIVE_TAG);
            enc.write_u8(version.as_wire());
        }
        BootstrapCodec::AgentEventsJsonlV1 => {
            enc.write_u8(BootstrapCodec::AGENT_EVENTS_JSONL_V1_TAG);
        }
    }
}

pub(in crate::wire) fn decode_bootstrap_codec(
    dec: &mut Decoder<'_>,
) -> Result<BootstrapCodec, DecodeError> {
    match dec.read_u8()? {
        BootstrapCodec::SYNTHESIZED_VT_V1_TAG => Ok(BootstrapCodec::SynthesizedVtV1),
        BootstrapCodec::NATIVE_TAG => {
            let value = dec.read_u8()?;
            let codec = EngineCodec::from_wire(value)
                .ok_or_else(|| DecodeError::unknown_enum("EngineCodec", value))?;
            Ok(BootstrapCodec::Native(codec))
        }
        BootstrapCodec::AGENT_EVENTS_JSONL_V1_TAG => Ok(BootstrapCodec::AgentEventsJsonlV1),
        value => Err(DecodeError::unknown_enum("BootstrapCodec", value)),
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
            let codec = EngineCodec::from_wire(value)
                .ok_or_else(|| DecodeError::unknown_enum("EngineCodec", value))?;
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
        value => Err(DecodeError::unknown_enum("BootstrapProfile", value)),
    }
}

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
        (BootstrapCodec::AgentEventsJsonlV1, 0, None) => {
            Ok(BootstrapStreamProfile::AgentEventsJsonlV1)
        }
        (BootstrapCodec::AgentEventsJsonlV1, 0, Some(after_seq)) => {
            Ok(BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq })
        }
        _ => Err(DecodeError::InvalidBootstrapProfile),
    }
}
