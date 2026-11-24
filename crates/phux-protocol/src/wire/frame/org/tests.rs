use super::*;

fn encoded(doc: &ArrangementDoc) -> Vec<u8> {
    let mut buf = BytesMut::new();
    encode_arrangement_doc(doc, &mut Encoder::new(&mut buf));
    buf.to_vec()
}

fn split_doc() -> ArrangementDoc {
    let leaf = |byte| {
        Box::new(ArrangementNode::Leaf(EntryRef::new(
            [byte; 16],
            EntryTarget::Member(ResourceKey::new([byte; 16]).expect("non-zero key")),
        )))
    };
    ArrangementDoc {
        groups: vec![ArrangementGroup {
            locked: false,
            id: [9; 16],
            name: Some("tab".to_owned()),
            tree: ArrangementNode::Split {
                dir: SplitDirection::Vertical,
                ratio: 0.25,
                first: leaf(1),
                second: leaf(2),
            },
        }],
    }
}

/// The `u32` length prefix at `at`.
fn len_at(bytes: &[u8], at: usize) -> usize {
    let raw: [u8; 4] = bytes[at..at + 4].try_into().expect("four bytes");
    u32::from_be_bytes(raw) as usize
}

/// `bytes` with the record at `at` grown by `extra` at its end, the
/// enclosing record at `outer` (if any) lengthened to match: what a
/// newer encoder appending a field would write.
fn grow(bytes: &[u8], at: usize, outer: Option<usize>, extra: &[u8]) -> Vec<u8> {
    let end = at + 4 + len_at(bytes, at);
    let mut out = [&bytes[..end], extra, &bytes[end..]].concat();
    for prefix in std::iter::once(at).chain(outer) {
        let len = u32::try_from(len_at(&out, prefix) + extra.len()).expect("small");
        out[prefix..prefix + 4].copy_from_slice(&len.to_be_bytes());
    }
    out
}

#[test]
fn bytes_a_newer_peer_appends_to_a_document_are_skipped() {
    let doc = split_doc();
    let bytes = encoded(&doc);
    assert_eq!(decode_arrangement_doc(&bytes), Ok(doc.clone()));

    // After the last group.
    let trailing = [bytes.as_slice(), &[0xEE; 5]].concat();
    assert_eq!(decode_arrangement_doc(&trailing), Ok(doc.clone()));

    // Inside the group record, which starts after the `u16` count: a newer
    // peer writes the lock byte, then whatever it added after it.
    let group = 2;
    let grown_group = grow(&bytes, group, None, &[0x00, 0xEE, 0xEE]);
    assert_eq!(decode_arrangement_doc(&grown_group), Ok(doc.clone()));
    let mut locked = doc.clone();
    locked.groups[0].locked = true;
    let locked_group = grow(&bytes, group, None, &[0x01, 0xEE]);
    assert_eq!(decode_arrangement_doc(&locked_group), Ok(locked.clone()));
    assert_eq!(encoded(&locked), grow(&bytes, group, None, &[0x01]));

    // Inside the split node, after the group's length, id, and name
    // (presence byte, `u32` length, "tab").
    let node = group + 4 + 16 + 1 + 4 + 3;
    let grown_node = grow(&bytes, node, Some(group), &[0xEE; 2]);
    assert_eq!(decode_arrangement_doc(&grown_node), Ok(doc));
}

/// Slot tabs ride at the end of the leaf record: a leaf without them
/// encodes as it always did, one with them round-trips, and a default
/// placed past its tabs is refused.
#[test]
fn slot_tabs_trail_the_leaf_and_round_trip() {
    let tabbed = |shown_at| {
        let mut doc = split_doc();
        if let ArrangementNode::Split { first, .. } = &mut doc.groups[0].tree
            && let ArrangementNode::Leaf(slot) = first.as_mut()
        {
            slot.tabs = vec![EntryRef::new(
                [7; 16],
                EntryTarget::Member(ResourceKey::new([7; 16]).expect("non-zero key")),
            )];
            slot.shown_at = shown_at;
        }
        doc
    };
    let doc = tabbed(1);
    let bytes = encoded(&doc);
    assert_eq!(decode_arrangement_doc(&bytes), Ok(doc));
    assert!(bytes.len() > encoded(&split_doc()).len());
    assert_eq!(
        decode_arrangement_doc(&encoded(&tabbed(2))),
        Err(DecodeError::InvalidArrangement)
    );
}

#[test]
fn terminal_launch_accepts_shell_defaults_empty_values_and_bounded_lists() {
    let launch = LaunchSpec::new(vec!["sh".into(), String::new()])
        .with_cwd(Some(std::env::temp_dir().to_string_lossy().into_owned()))
        .with_env(vec![
            ("LANG".into(), "C".into()),
            ("LANG".into(), String::new()),
        ])
        .with_term(Some("xterm-256color".into()))
        .with_restore(Some(vec![0, 255, 1]));
    assert_eq!(launch.validate_terminal(), Ok(()));
    assert_eq!(LaunchSpec::default().validate_terminal(), Ok(()));
    let bounded = LaunchSpec::new(vec!["x".into(); MAX_LAUNCH_LIST]).with_env(vec![
        (
            "X".into(),
            String::new()
        );
        MAX_LAUNCH_LIST
    ]);
    assert_eq!(bounded.validate_terminal(), Ok(()));
}

#[test]
fn terminal_launch_rejects_invalid_values_without_host_io() {
    let absolute_nul = format!("{}\0bad", std::path::MAIN_SEPARATOR);
    for launch in [
        LaunchSpec::new(vec![String::new()]),
        LaunchSpec::new(vec!["sh".into(), "bad\0arg".into()]),
        LaunchSpec::new(vec!["x".into(); MAX_LAUNCH_LIST + 1]),
        LaunchSpec::default().with_cwd(Some("relative".into())),
        LaunchSpec::default().with_cwd(Some(absolute_nul)),
        LaunchSpec::default().with_env(vec![("X".into(), String::new()); MAX_LAUNCH_LIST + 1]),
    ] {
        assert!(launch.validate_terminal().is_err(), "{launch:?}");
    }
    for (key, value) in [
        ("", "x"),
        ("A=B", "x"),
        ("A\0B", "x"),
        ("A", "x\0y"),
        ("PHUX_", "x"),
        ("PHUX_PRIVATE", "x"),
        ("TERM", "xterm"),
    ] {
        let launch = LaunchSpec::default().with_env(vec![(key.into(), value.into())]);
        assert!(launch.validate_terminal().is_err(), "{key:?}");
    }
}

#[test]
fn unknown_refusal_tags_and_verbs_preserve_the_exact_body() {
    // Independent positional bytes: tag, u32 body length, body. A future verb
    // under the known unauthorized tag is just as opaque as a future tag.
    for bytes in [
        vec![0xee, 0, 0, 0, 4, 0, 255, 17, 0],
        vec![0x00, 0, 0, 0, 3, 0xff, 12, 34],
    ] {
        let refusal = decode_refusal(&mut Decoder::new(&bytes)).expect("opaque refusal");
        assert_eq!(
            refusal,
            Refusal::Unknown {
                tag: bytes[0],
                body: bytes[5..].to_vec(),
            }
        );
        assert_eq!(refusal.as_str(), "unknown");
        assert_eq!(
            crate::api::ErrorCode::from_refusal(&refusal),
            crate::api::ErrorCode::Unknown
        );
        let mut encoded = BytesMut::new();
        encode_refusal(&refusal, &mut Encoder::new(&mut encoded));
        assert_eq!(encoded.as_ref(), bytes.as_slice());
    }
    // A truncated known body is malformed, never a new refusal or success.
    assert!(decode_refusal(&mut Decoder::new(&[0x02, 0, 0, 0, 1, 0])).is_err());
}

#[test]
fn wire_refusal_names_and_public_projection_remain_stable() {
    for (refusal, name) in [
        (Refusal::Unauthorized { verb: Verb::ALL[0] }, "unauthorized"),
        (Refusal::NotFound, "not_found"),
        (
            Refusal::Conflict {
                current: Revision::new(7),
            },
            "conflict",
        ),
        (Refusal::Stale { current: None }, "stale"),
        (Refusal::NotEmpty { members: 2 }, "not_empty"),
        (
            Refusal::Unsupported {
                capability: "future".into(),
            },
            "unsupported",
        ),
        (
            Refusal::InvalidArgument {
                field: "argv".into(),
                reason: "invalid".into(),
            },
            "invalid_argument",
        ),
        (Refusal::NameTaken, "name_taken"),
        (Refusal::ExecutionUncertain, "execution_uncertain"),
        (
            Refusal::LeaseHeld {
                holder: ActorRef::new(crate::ids::ClientId::new(1)),
            },
            "lease_held",
        ),
        (Refusal::DuplicateOperation, "duplicate_operation"),
        (
            Refusal::StoreFailure {
                reason: "unavailable".into(),
            },
            "store_failure",
        ),
        (
            Refusal::Unknown {
                tag: 255,
                body: vec![1, 2],
            },
            "unknown",
        ),
    ] {
        assert_eq!(refusal.as_str(), name);
        assert_eq!(crate::api::ErrorCode::from_refusal(&refusal).as_str(), name);
    }
}
