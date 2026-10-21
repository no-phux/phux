//! `HELLO` and `HELLO_OK`: readiness, version and limit checks, and the offer.

use super::*;

#[wasm_bindgen_test]
async fn hello_ok_without_aggregate_ready_is_not_connection_ready() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let outcome = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert!(outcome.fatal.is_none());
    assert_eq!(outcome.send.len(), 2, "HELLO_OK subscribes before attach");
    assert!(matches!(
        decode_one(&outcome.send[0]),
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None
        }
    ));
    assert!(!session.is_attach_ready(), "HELLO_OK alone is not usable");
}

#[wasm_bindgen_test]
async fn hello_ok_rejects_version_drift_and_oversized_limits_before_payload() {
    let vt = Vt::load().await.expect("load engine");

    let mut wrong_version = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    let FrameKind::HelloOk { protocol_major, .. } = &mut wrong_version else {
        unreachable!();
    };
    *protocol_major = protocol_major.saturating_add(1);
    let mut session = Connection::new(&vt, 80, 24);
    assert!(session.on_frame(wrong_version).fatal.is_some());

    let mut synthesized_only = Connection::new_synthesized_compat(&vt, 80, 24);
    let drift = synthesized_only.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(drift.fatal.is_some());
    assert!(
        drift.send.is_empty(),
        "drift must fail before ATTACH/payload"
    );
    assert!(synthesized_only.selected_profile().is_none());

    let oversized =
        BootstrapLimits::new(512 * 1024, 2 * 1024 * 1024).expect("within protocol hard limits");
    let mut session = Connection::new(&vt, 80, 24);
    assert!(
        session
            .on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, oversized))
            .fatal
            .is_some()
    );
}

#[wasm_bindgen_test]
async fn hello_advertises_official_snapshot_with_synthesized_fallback() {
    let vt = Vt::load().await.expect("load engine");
    assert!(
        vt.native_snapshot_available(),
        "the embedded engine decodes official GHOSTSNP"
    );

    let session = Connection::new(&vt, 80, 24);
    let frames = session.handshake();
    let (hello, rest) = FrameKind::decode(&frames[0]).expect("decode hello");
    assert!(rest.is_empty());
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first handshake frame must be HELLO");
    };
    assert_eq!(client_caps.image_protocols, ImageProtocolSet::new());
    let bootstrap = &client_caps.bootstrap;
    assert!(
        bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState)
    );
    assert!(
        bootstrap
            .native_codecs
            .contains(EngineCodec::LibghosttySnapshotV1)
    );
    assert!(
        !bootstrap
            .native_codecs
            .contains(EngineCodec::LibghosttyCheckpointV2),
        "the legacy checkpoint codec is never offered"
    );
    assert_eq!(
        bootstrap
            .native_features
            .intersect(EngineFeatureSet::required_native()),
        EngineFeatureSet::required_native()
    );
    for fallback in [
        BootstrapProfileKind::SynthesizedVtRaw,
        BootstrapProfileKind::SynthesizedVtStateSync,
    ] {
        assert!(bootstrap.profiles.contains(fallback), "{fallback:?}");
    }

    let compatibility = Connection::new_synthesized_compat(&vt, 80, 24);
    assert!(
        !compatibility
            .advertised_capabilities()
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState)
    );
    assert!(
        compatibility
            .advertised_capabilities()
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw)
    );
}

#[wasm_bindgen_test]
async fn session_accepts_native_snapshot_and_rejects_the_unadvertised_checkpoint() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let negotiated = session.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(negotiated.fatal.is_none());
    assert!(!negotiated.send.is_empty(), "native proceeds to ATTACH");
    assert_eq!(session.selected_profile(), Some(native_profile()));

    let mut session = Connection::new(&vt, 80, 24);
    let legacy = session.on_frame(hello_ok(
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
            features: EngineFeatureSet::required_native(),
        },
        BootstrapLimits::default(),
    ));
    assert!(legacy.fatal.is_some());
    assert!(
        legacy.send.is_empty(),
        "unadvertised native must fail before ATTACH/payload"
    );
    assert!(session.selected_profile().is_none());
}

fn hello_ok_version(major: u16, minor: u16, patch: u16) -> FrameKind {
    let mut frame = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    if let FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        ..
    } = &mut frame
    {
        (*protocol_major, *protocol_minor, *protocol_patch) = (major, minor, patch);
    }
    frame
}

#[wasm_bindgen_test]
async fn hello_ok_on_another_minor_is_accepted_and_recorded() {
    let vt = Vt::load().await.expect("load engine");
    let v = PROTOCOL_VERSION;
    let mut session = Connection::new(&vt, 80, 24);
    let outcome = session.on_frame(hello_ok_version(v.major, v.minor + 1, v.patch + 2));
    assert!(outcome.fatal.is_none(), "{:?}", outcome.fatal);
    let recorded = session.server_protocol().expect("recorded");
    assert_eq!(
        (recorded.major, recorded.minor, recorded.patch),
        (v.major, v.minor + 1, v.patch + 2)
    );
}

#[wasm_bindgen_test]
async fn a_protocol_major_refusal_stops_the_client_for_good() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 80, 24);
    let outcome = session.on_frame(hello_ok_version(PROTOCOL_VERSION.major + 1, 0, 0));
    let message = outcome.fatal.expect("another major is fatal");
    assert!(session.is_version_refused());
    assert!(phux_web::session::is_version_refusal(&message), "{message}");

    let mut session = Connection::new(&vt, 80, 24);
    let outcome = session.on_frame(FrameKind::Error {
        request_id: None,
        code: phux_protocol::wire::frame::ErrorCode::VersionIncompatible,
        message: "client protocol 9.0 unsupported".to_owned(),
    });
    let message = outcome.fatal.expect("VERSION_INCOMPATIBLE is fatal");
    assert!(session.is_version_refused());
    assert!(
        message.contains("client protocol 9.0 unsupported"),
        "{message}"
    );
    assert!(phux_web::session::is_version_refusal(&message), "{message}");
}
