//! Bootstrap profile selection between native and synthesized codecs.

use super::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfile, BootstrapProfileKind,
    BootstrapProfileSet, ClientCapabilities, DecodeError, EngineCodec, EngineCodecSet,
    EngineFeature, EngineFeatureSet, FrameKind, OutputMode, PROTOCOL_VERSION, TYPE_HELLO_OK,
    framed, select_bootstrap_profile, tlv_field,
};

#[test]
fn profile_selection_prefers_native_then_explicit_compatibility() {
    let server = BootstrapCapabilities::new()
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
        .with_limits(BootstrapLimits::new(64 * 1024, 256 * 1024).unwrap());
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_native(
                EngineCodec::LibghosttyCheckpointV2,
                EngineFeatureSet::required_native(),
            )
            .with_limits(BootstrapLimits::new(128 * 1024, 128 * 1024).unwrap()),
    );
    let (profile, limits) = select_bootstrap_profile(&client, &server).unwrap();
    assert_eq!(
        profile,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,

            features: EngineFeatureSet::required_native(),
        }
    );
    assert_eq!(limits.max_chunk_bytes(), 64 * 1024);
    assert_eq!(limits.max_history_page_bytes(), 128 * 1024);

    let state_sync_only =
        BootstrapProfileSet::with(&[BootstrapProfileKind::SynthesizedVtStateSync]);
    let client = ClientCapabilities::new()
        .with_output_mode(OutputMode::StateSync)
        .with_bootstrap(BootstrapCapabilities::new().with_profiles(state_sync_only));
    let server = BootstrapCapabilities::new().with_profiles(state_sync_only);
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtStateSync
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_native(
                EngineCodec::LibghosttyCheckpointV2,
                EngineFeatureSet::required_native(),
            )
            .with_profiles(native_only)
            .with_native_codecs(EngineCodecSet::new()),
    );
    let server = BootstrapCapabilities::new()
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
        .with_profiles(native_only);
    assert!(select_bootstrap_profile(&client, &server).is_err());
}

#[test]
fn native_selection_uses_the_highest_exact_known_common_codec() {
    let future_codec_bit = 1_u64 << 63;
    let v2_bit = 1_u64 << EngineCodec::LibghosttyCheckpointV2.as_wire();
    let snapshot_v1_bit = 1_u64 << EngineCodec::LibghosttySnapshotV1.as_wire();
    let client_codecs = EngineCodecSet::from_wire(future_codec_bit | v2_bit | snapshot_v1_bit);
    let server_codecs = EngineCodecSet::from_wire(v2_bit | snapshot_v1_bit);
    assert_eq!(
        client_codecs.highest_common(server_codecs),
        Some(EngineCodec::LibghosttySnapshotV1)
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(native_only)
            .with_native_codecs(client_codecs)
            .with_native_features(EngineFeatureSet::required_native()),
    );
    let server = BootstrapCapabilities::new()
        .with_profiles(native_only)
        .with_native_codecs(server_codecs)
        .with_native_features(EngineFeatureSet::required_native());
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        }
    );
}

#[test]
fn official_snapshot_v1_and_legacy_checkpoint_v2_fall_back_to_synthesis() {
    let client =
        ClientCapabilities::new().with_bootstrap(BootstrapCapabilities::new().with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        ));
    let server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    );

    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtRaw,
        "different native contracts must never be inferred compatible"
    );
    assert_eq!(EngineCodec::LibghosttyCheckpointV2.as_wire(), 2);
    assert_eq!(EngineCodec::LibghosttySnapshotV1.as_wire(), 4);
    assert_eq!(
        EngineCodec::from_wire(2),
        Some(EngineCodec::LibghosttyCheckpointV2),
        "legacy checkpoint-v2 remains decodable"
    );
}

/// Id 3 named the BLAKE3-checkpointed `GHOSTSNPv1` bytes that the current
/// engine can neither produce nor decode. A peer that still advertises only
/// id 3 must fall back to a synthesized profile, never select native.
#[test]
fn retired_snapshot_id_3_falls_back_to_synthesis() {
    assert_eq!(EngineCodec::from_wire(3), None);
    let retired_only = EngineCodecSet::from_wire(1_u64 << 3);
    assert_eq!(retired_only, EngineCodecSet::new());

    let advertised = BootstrapProfileSet::all();
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(advertised)
            .with_native_codecs(retired_only)
            .with_native_features(EngineFeatureSet::required_native()),
    );
    let server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttySnapshotV1,
        EngineFeatureSet::required_native(),
    );
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtRaw
    );
}

#[test]
fn future_only_codec_bits_never_select_native() {
    let future_only = EngineCodecSet::from_wire(1_u64 << 63);
    assert_eq!(future_only, EngineCodecSet::new());

    let advertised = BootstrapProfileSet::all();
    let client = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(advertised)
            .with_native_codecs(future_only)
            .with_native_features(EngineFeatureSet::required_native()),
    );
    let server = BootstrapCapabilities::new()
        .with_profiles(advertised)
        .with_native_codecs(future_only)
        .with_native_features(EngineFeatureSet::required_native());
    assert_eq!(
        select_bootstrap_profile(&client, &server).unwrap().0,
        BootstrapProfile::SynthesizedVtRaw
    );

    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let client =
        ClientCapabilities::new().with_bootstrap(client.bootstrap.with_profiles(native_only));
    let server = server.with_profiles(native_only);
    assert!(select_bootstrap_profile(&client, &server).is_err());
}

#[test]
fn every_native_required_feature_is_strict() {
    let incomplete_feature_sets = [
        EngineFeatureSet::with(&[
            EngineFeature::ReadyBoundary,
            EngineFeature::HistoryPages,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::HistoryPages,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::ReadyBoundary,
            EngineFeature::BoundedHistoryControl,
        ]),
        EngineFeatureSet::with(&[
            EngineFeature::Continuation,
            EngineFeature::ReadyBoundary,
            EngineFeature::HistoryPages,
        ]),
    ];
    let native_and_raw = BootstrapProfileSet::with(&[
        BootstrapProfileKind::NativeState,
        BootstrapProfileKind::SynthesizedVtRaw,
    ]);
    let native_only = BootstrapProfileSet::with(&[BootstrapProfileKind::NativeState]);
    let codecs = EngineCodecSet::with(&[EngineCodec::LibghosttyCheckpointV2]);
    let server = BootstrapCapabilities::new()
        .with_profiles(native_and_raw)
        .with_native_codecs(codecs)
        .with_native_features(EngineFeatureSet::required_native());

    for features in incomplete_feature_sets {
        let bootstrap = BootstrapCapabilities::new()
            .with_profiles(native_and_raw)
            .with_native_codecs(codecs)
            .with_native_features(features);
        let client = ClientCapabilities::new().with_bootstrap(bootstrap);
        assert_eq!(
            select_bootstrap_profile(&client, &server).unwrap().0,
            BootstrapProfile::SynthesizedVtRaw,
            "partial native features must fall back explicitly"
        );

        let native_required =
            ClientCapabilities::new().with_bootstrap(bootstrap.with_profiles(native_only));
        assert!(
            select_bootstrap_profile(&native_required, &server.with_profiles(native_only)).is_err(),
            "partial native features must reject when no synthesis profile is common"
        );
    }
}

#[test]
fn versioned_native_offer_forces_mixed_peers_to_synthesis_or_rejection() {
    const LEGACY_NATIVE_BIT: u8 = 0x01;
    const SYNTHESIZED_RAW_BIT: u8 = 0x02;
    const LEGACY_PROFILE_MASK: u8 = 0x07;

    let new_server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    let new_offer_as_seen_by_old_server = new_server.profiles.as_wire() & LEGACY_PROFILE_MASK;
    let old_server_selection = if new_offer_as_seen_by_old_server & LEGACY_NATIVE_BIT != 0 {
        LEGACY_NATIVE_BIT
    } else if new_offer_as_seen_by_old_server & SYNTHESIZED_RAW_BIT != 0 {
        SYNTHESIZED_RAW_BIT
    } else {
        0
    };
    assert_eq!(
        old_server_selection, SYNTHESIZED_RAW_BIT,
        "an old server must not mistake the versioned native offer for legacy native"
    );

    let mut legacy_hello_ok = Vec::new();
    tlv_field(&mut legacy_hello_ok, 1, &0_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 2, &7_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 3, &0_u16.to_be_bytes());
    tlv_field(&mut legacy_hello_ok, 4, &[1, 0, 0, 0, 0]);
    tlv_field(&mut legacy_hello_ok, 5, b"legacy-server");
    tlv_field(
        &mut legacy_hello_ok,
        6,
        &[0, EngineCodec::LibghosttyCheckpointV2.as_wire(), 0, 0, 0, 7],
    );
    assert_eq!(
        FrameKind::decode(&framed(TYPE_HELLO_OK, &legacy_hello_ok)).unwrap_err(),
        DecodeError::UnknownEnumValue {
            field: "BootstrapProfile",
            value: 0,
        },
        "a new client must never accept the retired selected-profile tag"
    );

    let old_client_profiles =
        BootstrapProfileSet::from_wire(LEGACY_NATIVE_BIT | SYNTHESIZED_RAW_BIT);
    let old_client = ClientCapabilities::new()
        .with_bootstrap(BootstrapCapabilities::new().with_profiles(old_client_profiles));
    assert_eq!(
        select_bootstrap_profile(&old_client, &new_server)
            .unwrap()
            .0,
        BootstrapProfile::SynthesizedVtRaw,
        "a new server must ignore the retired legacy native bit"
    );

    let old_native_only = ClientCapabilities::new().with_bootstrap(
        BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::from_wire(LEGACY_NATIVE_BIT)),
    );
    assert!(
        select_bootstrap_profile(&old_native_only, &new_server).is_err(),
        "legacy native without a shared synthesized profile must fail before attach"
    );
}

#[test]
fn old_engine_capabilities_fall_back_without_changing_protocol_version() {
    let old_engine_client = ClientCapabilities::new();
    let native_server = BootstrapCapabilities::new().with_native(
        EngineCodec::LibghosttyCheckpointV2,
        EngineFeatureSet::required_native(),
    );
    assert_eq!(
        select_bootstrap_profile(&old_engine_client, &native_server)
            .unwrap()
            .0,
        BootstrapProfile::SynthesizedVtRaw
    );
    assert_eq!((PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor), (0, 9));
    assert_eq!(EngineCodec::LibghosttyCheckpointV2.as_wire(), 2);
}
