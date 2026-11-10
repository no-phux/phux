//! The catalog's stream and spawn contracts agree with themselves, with the
//! codec enums, and with the lookups the decoder, relay and kernel use; and
//! a kind added with only a catalog row is classified by those lookups.

use super::*;
use crate::caps::{BootstrapCodec, BootstrapStreamProfile, EngineCodec};
use crate::wire::field::spawn_terminal as spawn_field;

#[test]
fn every_catalog_row_is_self_consistent() {
    for spec in &KINDS {
        let name = spec.name;
        let stream = &spec.stream;
        let spawn = &spec.spawn;
        assert!(core::ptr::eq(kind_spec(spec.kind).unwrap(), spec), "{name}");
        assert_eq!(spec.key, name.to_ascii_lowercase(), "{name}");
        assert_eq!(spec.kind.as_str(), spec.key, "{name}");
        assert!(
            !stream.codecs.is_empty() && !stream.delivery.is_empty(),
            "{name}"
        );
        // Only a Terminal is a root, measures cells, and is the
        // connection's negotiated profile.
        let terminal = spec.kind.is_terminal();
        assert_eq!(stream.needs_terminal_profile, terminal, "{name}");
        assert_eq!(spawn.parent == ParentRule::Forbidden, terminal, "{name}");
        assert_eq!(stream.geometry.in_bootstrap(), terminal, "{name}");
        assert_eq!(needs_terminal_profile(spec.kind), terminal, "{name}");
        // A cursor resumes a log.
        assert!(
            stream.resume.is_none() || stream.delivers(Delivery::Log),
            "{name}"
        );
        assert_eq!(
            spec.kind.is_document(),
            stream.delivers(Delivery::WholeDocument)
        );
        for codec in stream.codecs {
            // A kind-fixed codec is gated like its kind; a Terminal's is
            // selected by the connection's profile instead.
            let gate = codec.gate.map(FeatureGate::wire_name);
            let want = if terminal {
                None
            } else {
                spec.gate.map(FeatureGate::wire_name)
            };
            assert_eq!(gate, want, "{name} {}", codec.name);
        }
        // A projection names its producer.
        let projection = spawn.parent == ParentRule::Required;
        assert_eq!(
            spawn.provider.presence == Presence::Required,
            projection,
            "{name}"
        );
        for facet in [spawn.provider, spawn.native_id] {
            assert!(facet.max_bytes > 0 && !facet.meaning.is_empty(), "{name}");
        }
        assert!(spawn.provider.max_bytes <= max_spawn_provider_bytes());
        assert!(spawn.native_id.max_bytes <= max_spawn_native_id_bytes());
        let mut seen = Vec::new();
        for id in spawn.forbidden {
            assert!(
                ![
                    spawn_field::PARENT,
                    spawn_field::PROVIDER,
                    spawn_field::NATIVE_ID
                ]
                .contains(id),
                "{name}: field {id} has a facet rule of its own"
            );
            assert!(!seen.contains(id), "{name}: field {id} listed twice");
            seen.push(*id);
        }
    }
}

#[test]
fn each_codec_tag_belongs_to_one_kind_and_names_its_variant() {
    let mut seen = Vec::new();
    for spec in &KINDS {
        for codec in spec.stream.codecs {
            assert!(!seen.contains(&codec.tag), "codec tag {} reused", codec.tag);
            seen.push(codec.tag);
            assert!(core::ptr::eq(codec_kind(codec.tag).unwrap(), spec));
            assert!(BootstrapCodec::is_allocated_tag(codec.tag));
            if codec.tag != BootstrapCodec::NATIVE_TAG {
                let decoded = BootstrapCodec::from_tag(codec.tag);
                assert_eq!(decoded.tag(), codec.tag, "{}", codec.name);
                assert!(
                    !matches!(
                        decoded,
                        BootstrapCodec::Unknown { .. } | BootstrapCodec::Kind { .. }
                    ),
                    "{} has a variant of its own",
                    codec.name
                );
            }
        }
    }
    assert!(!BootstrapCodec::is_allocated_tag(2), "tag 2 stays skipped");
    assert_eq!(
        BootstrapCodec::from_tag(200),
        BootstrapCodec::Unknown { tag: 200 }
    );
}

#[test]
fn every_stream_profile_names_the_kind_that_streams_it() {
    let native = EngineCodec::LibghosttySnapshotV1;
    for (profile, kind) in [
        (
            BootstrapStreamProfile::NativeState { codec: native },
            ResourceKind::Terminal,
        ),
        (
            BootstrapStreamProfile::SynthesizedVtRaw,
            ResourceKind::Terminal,
        ),
        (
            BootstrapStreamProfile::SynthesizedVtStateSync,
            ResourceKind::Terminal,
        ),
        (
            BootstrapStreamProfile::AgentEventsJsonlV1,
            ResourceKind::AgentSession,
        ),
        (
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 3 },
            ResourceKind::AgentSession,
        ),
        (
            BootstrapStreamProfile::AccessibilityTreeJsonV1,
            ResourceKind::AccessibilityTree,
        ),
        (
            BootstrapStreamProfile::PixelSurfacePngV1,
            ResourceKind::PixelSurface,
        ),
        (
            BootstrapStreamProfile::DocumentTypedV1,
            ResourceKind::Document,
        ),
    ] {
        assert_eq!(profile.kind(), Some(kind), "{profile:?}");
        let spec = profile.kind_spec().unwrap();
        if !spec.stream.needs_terminal_profile {
            assert_eq!(
                BootstrapStreamProfile::for_kind_codec_in(
                    &KINDS,
                    profile.codec(),
                    profile.resume_after_seq()
                ),
                Some(profile)
            );
        }
    }
    assert_eq!(
        BootstrapStreamProfile::Unknown { codec_tag: 9 }.kind(),
        None
    );
    // Only a kind whose contract resumes takes a cursor.
    assert_eq!(
        BootstrapStreamProfile::for_kind_codec_in(&KINDS, BootstrapCodec::DocumentTypedV1, Some(1)),
        None
    );
}

/// A hypothetical kind, tag 9, that this build has no variant for: only a
/// catalog row.
static FIXTURE: [KindSpec; 2] = [
    KINDS[0],
    KindSpec {
        kind: ResourceKind::Unknown { tag: 9 },
        name: "HYPOTHETICAL",
        key: "hypothetical",
        gate: Some(FIXTURE_GATE),
        leaf: true,
        methods: &[],
        events: &[],
        metadata_keys: &[],
        stream: whole_document_stream(
            &[StreamCodec {
                tag: 9,
                name: "HYPOTHETICAL_V1",
                gate: Some(FIXTURE_GATE),
            }],
            Geometry::Pixels,
        ),
        spawn: projection_spawn(
            named_producer("what it observes"),
            "its name",
            &PROJECTION_LAUNCH_FIELDS,
            None,
        ),
    },
];

const FIXTURE_GATE: FeatureGate = FeatureGate::FeaturesExt {
    mask: 1 << 30,
    wire_name: "HYPOTHETICAL",
};

#[test]
fn a_kind_with_only_a_catalog_row_is_decoded_and_classified() {
    let hypothetical = ResourceKind::Unknown { tag: 9 };
    // Decode: its codec tag is a catalog codec, not an unknown one.
    let codec = BootstrapCodec::from_tag_in(&FIXTURE, 9);
    assert_eq!(codec, BootstrapCodec::Kind { tag: 9 });
    assert_eq!(
        BootstrapCodec::from_tag(9),
        BootstrapCodec::Unknown { tag: 9 }
    );
    let profile = BootstrapStreamProfile::for_kind_codec_in(&FIXTURE, codec, None).unwrap();
    assert_eq!(
        profile,
        BootstrapStreamProfile::Kind {
            codec_tag: 9,
            after_seq: None
        }
    );
    assert_eq!(profile.codec(), codec);
    assert_eq!(profile.output_mode(), crate::caps::OutputMode::Raw);
    // It does not resume, so a cursor is refused.
    assert_eq!(
        BootstrapStreamProfile::for_kind_codec_in(&FIXTURE, codec, Some(1)),
        None
    );
    // Relay and attach: its stream needs no Terminal profile, its codec is
    // gated, and it is a whole document laid out in a leaf, `0 x 0`.
    let spec = codec_kind_in(&FIXTURE, 9).unwrap();
    assert!(core::ptr::eq(
        kind_spec_in(&FIXTURE, hypothetical).unwrap(),
        spec
    ));
    assert!(!spec.stream.needs_terminal_profile);
    assert_eq!(
        spec.stream
            .codec(9)
            .and_then(|codec| codec.gate)
            .map(FeatureGate::mask),
        Some(1 << 30)
    );
    assert!(spec.stream.delivers(Delivery::WholeDocument) && spec.leaf);
    assert!(!spec.stream.geometry.in_bootstrap());
    // Spawn: a projection under a parent, refusing launch fields.
    assert_eq!(spec.spawn.parent, ParentRule::Required);
    assert!(spec.spawn.forbidden.contains(&spawn_field::COMMAND));
}
