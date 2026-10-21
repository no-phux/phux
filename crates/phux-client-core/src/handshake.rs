//! Shared `HELLO_OK` acceptance for every frontend.
//!
//! The TUI attach path, the native FFI bridge, and other tokio-free hosts
//! must refuse the same handshakes: a protocol outside the client's major
//! line, a bootstrap profile outside the client's offer (native feature
//! intersection included), and payload limits that exceed the offer.
//! Minor and patch differences are accepted in both directions: a 1.x
//! client interoperates with any 1.x server and degrades by the server's
//! feature bits and the version it answered with, not by refusing.
//! Keeping the rule here is what stops Cockpit from accepting a `HELLO_OK`
//! the TUI would refuse.

use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    ClientCapabilities,
};
use phux_protocol::{PROTOCOL_VERSION, Version};
use thiserror::Error;

/// Whether a peer speaking `version` is on this client's compatibility line.
///
/// Semver: the major is the line. Any minor or patch, older or newer,
/// interoperates; additive behavior is gated by feature bits.
#[must_use]
pub const fn protocol_compatible(version: Version) -> bool {
    version.major == PROTOCOL_VERSION.major
}

/// Why a `HELLO_OK` did not answer the client's advertised handshake.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HelloOkError {
    /// Selected protocol is outside this client's major line
    /// (the major of [`phux_protocol::PROTOCOL_VERSION`]).
    #[error(
        "HELLO_OK selected incompatible protocol {major}.{minor}.{patch}; client speaks {}.x ({}.{}.{})",
        PROTOCOL_VERSION.major,
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
/// Only the major must match ([`protocol_compatible`]); a server on an
/// older or newer minor or patch is accepted. A native profile is accepted
/// only when every selected engine feature is in the offer
/// (`offered.native_features.intersect(features) == features`).
pub fn validate_hello_ok(
    offered: &ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    selected_limits: BootstrapLimits,
) -> Result<(), HelloOkError> {
    let selected = Version {
        major: protocol_major,
        minor: protocol_minor,
        patch: protocol_patch,
    };
    if !protocol_compatible(selected) {
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
    fn hello_ok_accepts_older_and_newer_minor_and_patch() {
        let v = PROTOCOL_VERSION;
        let same_line = [
            (v.minor, v.patch.wrapping_add(1)),
            (v.minor, v.patch.wrapping_sub(1)),
            (v.minor.wrapping_add(1), 0),
            (v.minor.wrapping_sub(1), 7),
        ];
        for (minor, patch) in same_line {
            assert_eq!(
                validate_hello_ok(
                    &matching_native_offer(),
                    v.major,
                    minor,
                    patch,
                    native_selection(),
                    BootstrapLimits::default(),
                ),
                Ok(()),
                "{}.{minor}.{patch} is on the client's major line",
                v.major
            );
        }
    }

    #[test]
    fn hello_ok_refuses_protocol_major_mismatch() {
        for major in [
            PROTOCOL_VERSION.major.wrapping_add(1),
            PROTOCOL_VERSION.major.wrapping_sub(1),
        ] {
            let err = validate_hello_ok(
                &matching_native_offer(),
                major,
                PROTOCOL_VERSION.minor,
                PROTOCOL_VERSION.patch,
                native_selection(),
                BootstrapLimits::default(),
            )
            .expect_err("major mismatch must refuse");
            assert!(
                matches!(err, HelloOkError::UnsupportedProtocol { major: m, .. } if m == major),
                "got {err:?}"
            );
        }
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
