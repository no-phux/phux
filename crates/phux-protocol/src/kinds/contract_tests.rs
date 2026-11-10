//! The catalog's stream and spawn contracts agree with themselves and with
//! the codec enums; and a kind added with only a catalog row is decoded,
//! classified, validated and admitted by the same catalog-parameterised
//! functions the decoder, classifier and relay call with [`KINDS`].

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
        // A projection names its producer; only a projection opts into the
        // parent rows.
        let projection = spawn.parent == ParentRule::Required;
        assert!(!spawn.scoped_child || projection, "{name}");
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

/// The public names `ResourceKind::as_str` and `Display` print, pinned.
#[test]
fn kind_names_are_pinned() {
    let names: Vec<&str> = KINDS.iter().map(|spec| spec.kind.as_str()).collect();
    assert_eq!(
        names,
        [
            "terminal",
            "agent_session",
            "accessibility_tree",
            "pixel_surface",
            "document"
        ]
    );
    assert_eq!(ResourceKind::Unknown { tag: 9 }.as_str(), "unknown");
    assert_eq!(ResourceKind::Unknown { tag: 9 }.to_string(), "unknown(9)");
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

/// Two hypothetical kinds this build has no variant for, each only a
/// catalog row: tag 9 opts into the parent rows, tag 10 does not.
static FIXTURE: [KindSpec; 3] = [
    KINDS[0],
    fixture_kind(9, "HYPOTHETICAL", true),
    fixture_kind(10, "UNOPTED", false),
];

/// A gate on a bit the typed feature sets keep: an unknown one is dropped.
const FIXTURE_GATE: FeatureGate = FeatureGate::FeaturesExt {
    mask: ServerFeatureExt::TypedDocument.mask(),
    wire_name: "HYPOTHETICAL",
};

const fn fixture_kind(tag: u8, name: &'static str, scoped_child: bool) -> KindSpec {
    KindSpec {
        kind: ResourceKind::Unknown { tag },
        name,
        key: "hypothetical",
        gate: Some(FIXTURE_GATE),
        leaf: true,
        methods: &[],
        events: &[],
        metadata_keys: &[],
        stream: whole_document_stream(
            match tag {
                9 => &[StreamCodec {
                    tag: 9,
                    name: "HYPOTHETICAL_V1",
                    gate: Some(FIXTURE_GATE),
                }],
                _ => &[StreamCodec {
                    tag: 10,
                    name: "UNOPTED_V1",
                    gate: Some(FIXTURE_GATE),
                }],
            },
            Geometry::Pixels,
        ),
        spawn: projection_spawn(
            named_producer("what it observes"),
            "its name",
            &PROJECTION_LAUNCH_FIELDS,
            None,
            scoped_child,
        ),
    }
}

fn fixture_spawn(tag: u8, satellite: Option<&str>, command: bool) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: crate::ids::GroupId::new(1),
        command: command.then(|| vec!["sh".to_owned()]),
        cwd: None,
        env: None,
        term: None,
        satellite: satellite.map(SatelliteHost::new),
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: Some(Box::new(SpawnResource {
            kind: ResourceKind::Unknown { tag },
            ..SpawnResource::agent_session(ResourceId::local(7), "what it observes")
        })),
    }
}

/// Decode, relay admission, spawn validation and spawn classification of a
/// kind that is only a catalog row, through the functions the decoder,
/// relay and classifier call with [`KINDS`].
#[test]
fn a_kind_with_only_a_catalog_row_is_decoded_validated_admitted_and_classified() {
    let hypothetical = ResourceKind::Unknown { tag: 9 };
    // Decode: the codec tag is a catalog codec, not an unknown one, and its
    // stream profile is the kind's raw stream without a cursor.
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
    assert_eq!(
        BootstrapStreamProfile::for_kind_codec_in(&FIXTURE, codec, Some(1)),
        None
    );
    let spec = codec_kind_in(&FIXTURE, 9).unwrap();
    assert!(core::ptr::eq(
        kind_spec_in(&FIXTURE, hypothetical).unwrap(),
        spec
    ));
    assert!(!spec.stream.needs_terminal_profile);
    assert!(!spec.stream.geometry.in_bootstrap());

    // Relay: admitted only when the satellite advertises the codec's gate,
    // and never with a cursor, since the kind does not resume.
    let none = crate::caps::ServerFeatureSet::new();
    let gated = crate::caps::ServerFeatureExtSet::from_wire(FIXTURE_GATE.mask());
    let ungated = crate::caps::ServerFeatureExtSet::new();
    assert!(spec.stream.admits(9, false, none, gated));
    assert!(!spec.stream.admits(9, false, none, ungated));
    assert!(!spec.stream.admits(9, true, none, gated));
    assert!(!spec.stream.admits(10, false, none, gated));

    // Spawn validation: a projection's launch fields are refused.
    let validate = crate::wire::decode::validate_spawn_for_kind_in;
    assert_eq!(validate(&FIXTURE, &fixture_spawn(9, None, false)), Ok(()));
    assert_eq!(
        validate(&FIXTURE, &fixture_spawn(9, None, true)),
        Err(crate::wire::error::DecodeError::InvalidSpawnForKind {
            kind: 9,
            field: spawn_field::COMMAND,
            required: false,
        })
    );

    // Classification: the opted-in kind lands on the parent rows; the
    // other starts denied.
    let row = |frame: &FrameKind| {
        let FrameKind::SpawnResource {
            satellite,
            owner_terminal,
            resource,
            ..
        } = frame
        else {
            unreachable!()
        };
        spawn_rule_in(
            &FIXTURE,
            satellite.as_ref(),
            owner_terminal.is_some(),
            resource.as_deref(),
        )
        .case
    };
    assert_eq!(
        row(&fixture_spawn(9, None, false)),
        F_SPAWN_CHILD_LOCAL.case
    );
    assert_eq!(row(&fixture_spawn(10, None, false)), F_UNCLASSIFIED.case);
    // The real catalog serves neither, so both start denied there.
    assert_eq!(
        frame_rule(&fixture_spawn(9, None, false)).case,
        F_UNCLASSIFIED.case
    );
}
