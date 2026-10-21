use super::*;
use bytes::BytesMut;

fn body(build: impl FnOnce(&mut Encoder<'_>)) -> Vec<u8> {
    let mut buf = BytesMut::new();
    build(&mut Encoder::new(&mut buf));
    buf.to_vec()
}

#[test]
fn browse_and_satellite_search_round_trip() {
    for (recursive, host) in [(false, None), (true, Some(SatelliteHost::new("dev")))] {
        let bytes = body(|e| encode_query(e, 42, "~/src", "cfg", recursive, host.as_ref()));
        assert_eq!(
            decode_query(&mut Decoder::new(&bytes)),
            Ok((42, "~/src".into(), "cfg".into(), recursive, host))
        );
    }
}

#[test]
fn frames_dispatch_with_distinct_l3_tags() {
    let frames = [
        super::super::FrameKind::PathQuery {
            request_id: 7,
            root: "/srv".into(),
            query: "cfg".into(),
            recursive: true,
            host: Some(SatelliteHost::new("dev")),
        },
        super::super::FrameKind::PathResults {
            request_id: 7,
            result: Ok(PathResults {
                root: "/srv".into(),
                parent: Some("/".into()),
                rows: vec![PathRow {
                    path: "/srv/config.rs".into(),
                    kind: PathKind::File,
                }],
                status: PathStatus::Complete,
            }),
        },
    ];
    for frame in frames {
        let mut bytes = BytesMut::new();
        frame.encode(&mut bytes);
        let (decoded, rest) = super::super::FrameKind::decode(&bytes).expect("frame");
        assert_eq!(decoded, frame);
        assert!(rest.is_empty());
    }
}

#[test]
fn feature_extension_round_trips_in_hello_ok_without_changing_word_zero() {
    use crate::caps::{
        BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt,
        ServerFeatureExtSet, ServerFeatureSet,
    };

    let caps = ServerCapabilities::new()
        .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]));
    let frame = super::super::FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        server_caps: caps,
        server_id: vec![1],
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    let (decoded, rest) = super::super::FrameKind::decode(&bytes).expect("caps frame");
    assert!(rest.is_empty());
    assert_eq!(decoded, frame);
    let super::super::FrameKind::HelloOk { server_caps, .. } = decoded else {
        panic!("HELLO_OK")
    };
    assert_eq!(server_caps.features, ServerFeatureSet::new());
    assert!(
        server_caps
            .features_ext
            .contains(ServerFeatureExt::PathQuery)
    );
}

#[test]
fn complete_warming_truncated_and_refusal_round_trip() {
    for status in [
        PathStatus::Complete,
        PathStatus::Warming,
        PathStatus::Truncated,
    ] {
        let reply = Ok(PathResults {
            root: "/home/é".into(),
            parent: Some("/home".into()),
            rows: vec![
                PathRow {
                    path: "/home/é/a.rs".into(),
                    kind: PathKind::File,
                },
                PathRow {
                    path: "/home/é/dir".into(),
                    kind: PathKind::Directory,
                },
                PathRow {
                    path: "/home/é/link".into(),
                    kind: PathKind::Symlink,
                },
            ],
            status,
        });
        let bytes = body(|e| encode_results(e, 4, &reply));
        assert_eq!(decode_results(&mut Decoder::new(&bytes)), Ok((4, reply)));
    }
    let refusal = Err(PathQueryError {
        root: "/private".into(),
        code: PathErrorCode::PermissionDenied,
        message: "permission denied".into(),
    });
    let bytes = body(|e| encode_results(e, 9, &refusal));
    assert_eq!(decode_results(&mut Decoder::new(&bytes)), Ok((9, refusal)));
}

#[test]
fn unknown_fields_are_skipped_but_invalid_known_fields_fail() {
    let bytes = body(|e| {
        encode_query(e, 1, "/", "a", false, None);
        e.write_field(99, b"future");
    });
    assert_eq!(
        decode_query(&mut Decoder::new(&bytes)),
        Ok((1, "/".into(), "a".into(), false, None))
    );
    let bytes = body(|e| {
        encode_query(e, 1, "/", "a", false, None);
        e.write_field(field::path_query::RECURSIVE, &[2]);
    });
    assert!(decode_query(&mut Decoder::new(&bytes)).is_err());
    let bytes = body(|e| {
        e.write_field_with(field::path_query::REQUEST_ID, |f| f.write_u32_be(1));
        e.write_field(field::path_query::ROOT, b"/");
    });
    assert!(decode_query(&mut Decoder::new(&bytes)).is_err());
}

#[test]
fn row_count_bound_and_malformed_row_data() {
    let at_bound = body(|e| {
        e.write_u32_be(u32::try_from(MAX_PATH_RESULTS).expect("bounded"));
        for _ in 0..MAX_PATH_RESULTS {
            e.write_record(|e| {
                e.write_str("/a");
                e.write_u8(0);
            });
        }
    });
    assert_eq!(decode_rows(&at_bound).unwrap().0.len(), MAX_PATH_RESULTS);
    assert_eq!(
        decode_rows(&(u32::try_from(MAX_PATH_RESULTS).expect("bounded") + 1).to_be_bytes()),
        Err(DecodeError::PathResultLimitExceeded)
    );
    // Bytes after the list, or at the end of a row record, are a newer
    // peer's additions and are skipped.
    let mut trailing = body(|e| {
        e.write_u32_be(1);
        e.write_record(|e| {
            e.write_str("/a");
            e.write_u8(0);
            e.write_u8(0xEE);
        });
    });
    trailing.push(1);
    assert_eq!(decode_rows(&trailing).unwrap().0[0].path, "/a");
    let relative = body(|e| {
        e.write_u32_be(1);
        e.write_record(|e| {
            e.write_str("relative");
            e.write_u8(0);
        });
    });
    assert!(decode_rows(&relative).is_err());
}

#[test]
fn missing_status_conflicting_refusal_and_unknown_status_fail() {
    let missing = body(|e| {
        e.write_field_with(field::path_results::REQUEST_ID, |f| f.write_u32_be(1));
        e.write_field(field::path_results::ROOT, b"/");
        e.write_field_with(field::path_results::ROWS, |f| f.write_u32_be(0));
    });
    assert!(decode_results(&mut Decoder::new(&missing)).is_err());
    let conflicting = body(|e| {
        encode_results(
            e,
            1,
            &Err(PathQueryError {
                root: "/".into(),
                code: PathErrorCode::Other,
                message: "no".into(),
            }),
        );
        e.write_field_with(field::path_results::ROWS, |f| f.write_u32_be(0));
    });
    assert!(decode_results(&mut Decoder::new(&conflicting)).is_err());
    let unknown = body(|e| {
        encode_results(
            e,
            1,
            &Ok(PathResults {
                root: "/".into(),
                parent: None,
                rows: vec![],
                status: PathStatus::Complete,
            }),
        );
        e.write_field(field::path_results::STATUS, &[9]);
    });
    // A second status field is malformed, whatever its value.
    assert!(decode_results(&mut Decoder::new(&unknown)).is_err());
}

/// A newer server's status reads as not exhaustive, and a row of a kind this
/// build does not know is dropped and marks the reply not exhaustive.
#[test]
fn unknown_status_and_row_kind_degrade_instead_of_failing() {
    let reply = |status: u8, kinds: &[u8]| {
        body(|e| {
            e.write_field_with(field::path_results::REQUEST_ID, |f| f.write_u32_be(1));
            e.write_field(field::path_results::ROOT, b"/");
            e.write_field_with(field::path_results::ROWS, |f| {
                f.write_u32_be(u32::try_from(kinds.len()).expect("bounded"));
                for kind in kinds {
                    f.write_record(|f| {
                        f.write_str("/a");
                        f.write_u8(*kind);
                    });
                }
            });
            e.write_field(field::path_results::STATUS, &[status]);
        })
    };
    let decoded = |bytes: &[u8]| {
        let (request_id, result) = decode_results(&mut Decoder::new(bytes)).unwrap();
        assert_eq!(request_id, 1);
        result.unwrap()
    };
    assert_eq!(decoded(&reply(9, &[0])).status, PathStatus::Truncated);
    let dropped = decoded(&reply(0, &[0, 9, 1]));
    assert_eq!(
        dropped.rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        [PathKind::File, PathKind::Directory]
    );
    assert_eq!(dropped.status, PathStatus::Truncated);
    assert_eq!(decoded(&reply(1, &[9])).status, PathStatus::Warming);
    assert_eq!(decoded(&reply(0, &[2])).status, PathStatus::Complete);
}

#[test]
fn successful_results_reject_nonabsolute_or_nul_root_and_parent() {
    for (root, parent) in [
        ("relative", None),
        ("/okay\0bad", None),
        ("/okay", Some("relative")),
        ("/okay", Some("/parent\0bad")),
    ] {
        let reply = Ok(PathResults {
            root: root.into(),
            parent: parent.map(str::to_owned),
            rows: vec![],
            status: PathStatus::Complete,
        });
        let bytes = body(|encoder| encode_results(encoder, 1, &reply));
        assert!(decode_results(&mut Decoder::new(&bytes)).is_err());
    }
}
