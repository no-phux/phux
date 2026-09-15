//! Shared `HELLO_OK` acceptance for every frontend.
//!
//! The TUI attach path, the native FFI bridge, and other tokio-free hosts
//! must refuse the same handshakes: exact `PROTOCOL_VERSION` (including
//! patch), a bootstrap profile inside the client's offer (native feature
//! intersection included), and payload limits that do not exceed the offer.
//! Keeping the rule here is what stops Cockpit from accepting a `HELLO_OK`
//! the TUI would refuse.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    ClientCapabilities,
};
use thiserror::Error;

/// Why a `HELLO_OK` did not answer the client's advertised handshake.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HelloOkError {
    /// Selected protocol triple is not this client's [`phux_protocol::PROTOCOL_VERSION`].
    #[error(
        "HELLO_OK selected unsupported protocol {major}.{minor}.{patch}; client offered {}.{}.{}",
        PROTOCOL_VERSION.major,
        PROTOCOL_VERSION.minor,
        PROTOCOL_VERSION.patch
    )]
    UnsupportedProtocol {
        /// Server-selected major.
        major: u16,
        /// Server-selected minor.
        minor: u16,
        /// Server-selected patch.
        patch: u16,
    },
    /// Selected bootstrap profile is outside the client's offer.
    #[error("HELLO_OK selected bootstrap profile outside the client's offer: {0:?}")]
    UnadvertisedProfile(BootstrapProfile),
    /// Selected payload limits exceed the client's offer.
    #[error(
        "HELLO_OK selected bootstrap limits outside the client's offer: chunk={chunk} history_page={history_page}"
    )]
    LimitsOutsideOffer {
        /// Selected bootstrap chunk bound.
        chunk: u32,
        /// Selected history page bound.
        history_page: u32,
    },
}

/// Accept `HELLO_OK` only on the terms the client advertised.
///
/// Patch is part of the match: a peer that selected a different
/// `major.minor.patch` is refused, even when major and minor agree. A
/// native profile is accepted only when every selected engine feature is
/// in the offer (`offered.native_features.intersect(features) == features`).
pub fn validate_hello_ok(
    offered: &ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    selected_limits: BootstrapLimits,
) -> Result<(), HelloOkError> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err(HelloOkError::UnsupportedProtocol {
            major: protocol_major,
            minor: protocol_minor,
            patch: protocol_patch,
        });
    }

    if !profile_is_offered(&offered.bootstrap, selected_profile) {
        return Err(HelloOkError::UnadvertisedProfile(selected_profile));
    }

    if offered.bootstrap.limits.intersect(selected_limits) != selected_limits {
        return Err(HelloOkError::LimitsOutsideOffer {
            chunk: selected_limits.max_chunk_bytes(),
            history_page: selected_limits.max_history_page_bytes(),
        });
    }
    Ok(())
}

fn profile_is_offered(offered: &BootstrapCapabilities, selected_profile: BootstrapProfile) -> bool {
    match selected_profile {
        BootstrapProfile::NativeState { codec, features } => {
            offered.profiles.contains(BootstrapProfileKind::NativeState)
                && offered.native_codecs.contains(codec)
                && features.supports_native()
                && offered.native_features.intersect(features) == features
        }
        BootstrapProfile::SynthesizedVtRaw => offered
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw),
        BootstrapProfile::SynthesizedVtStateSync => offered
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::{
        BootstrapCapabilities, BootstrapProfileSet, EngineCodec, EngineCodecSet, EngineFeature,
        EngineFeatureSet,
    };

    fn matching_native_offer() -> ClientCapabilities {
        ClientCapabilities::new().with_bootstrap(BootstrapCapabilities::new().with_native(
            EngineCodec::LibghosttySnapshotV1,
            EngineFeatureSet::required_native(),
        ))
    }

    fn native_selection() -> BootstrapProfile {
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        }
    }

    #[test]
    fn hello_ok_accepts_matching_patch_and_native_features() {
        assert_eq!(
            validate_hello_ok(
                &matching_native_offer(),
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                native_selection(),
                BootstrapLimits::default(),
            ),
            Ok(())
        );
    }

    #[test]
    fn hello_ok_refuses_protocol_patch_mismatch() {
        let err = validate_hello_ok(
            &matching_native_offer(),
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch.wrapping_add(1),
            native_selection(),
            BootstrapLimits::default(),
        )
        .expect_err("patch mismatch must refuse");
        assert!(
            matches!(
                err,
                HelloOkError::UnsupportedProtocol { patch, .. }
                if patch == PROTOCOL_VERSION.patch.wrapping_add(1)
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn hello_ok_refuses_native_profile_when_required_features_are_missing() {
        let offered = ClientCapabilities::new().with_bootstrap(
            BootstrapCapabilities::new()
                .with_profiles(BootstrapProfileSet::with(&[
                    BootstrapProfileKind::NativeState,
                ]))
                .with_native_codecs(EngineCodecSet::with(&[EngineCodec::LibghosttySnapshotV1]))
                .with_native_features(EngineFeatureSet::with(&[EngineFeature::Continuation])),
        );
        let err = validate_hello_ok(
            &offered,
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
            native_selection(),
            BootstrapLimits::default(),
        )
        .expect_err("missing native features must refuse");
        assert!(
            matches!(
                err,
                HelloOkError::UnadvertisedProfile(BootstrapProfile::NativeState { .. })
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn hello_ok_profile_must_have_been_offered() {
        let offered = ClientCapabilities::new().with_bootstrap(
            BootstrapCapabilities::new()
                .with_profiles(BootstrapProfileSet::with(&[
                    BootstrapProfileKind::SynthesizedVtRaw,
                ]))
                .with_native_codecs(EngineCodecSet::new())
                .with_native_features(EngineFeatureSet::new()),
        );
        let malicious = BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        };
        assert!(matches!(
            validate_hello_ok(
                &offered,
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                malicious,
                BootstrapLimits::default(),
            ),
            Err(HelloOkError::UnadvertisedProfile(_))
        ));
    }

    #[test]
    fn hello_ok_limits_must_not_exceed_the_offer() {
        let offered = ClientCapabilities::new();
        let excessive = BootstrapLimits::new(512 * 1024, 2 * 1024 * 1024).expect("valid limits");
        assert!(matches!(
            validate_hello_ok(
                &offered,
                PROTOCOL_VERSION.major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                BootstrapProfile::SynthesizedVtRaw,
                excessive,
            ),
            Err(HelloOkError::LimitsOutsideOffer { .. })
        ));
    }
}
