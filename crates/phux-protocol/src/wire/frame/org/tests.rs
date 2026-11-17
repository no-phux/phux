use super::*;

fn encoded(doc: &ArrangementDoc) -> Vec<u8> {
    let mut buf = BytesMut::new();
    encode_arrangement_doc(doc, &mut Encoder::new(&mut buf));
    buf.to_vec()
}

fn split_doc() -> ArrangementDoc {
    let leaf = |byte| {
        Box::new(ArrangementNode::Leaf(EntryRef {
            entry: [byte; 16],
            target: EntryTarget::Member(ResourceKey::new([byte; 16]).expect("non-zero key")),
        }))
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
        stacks: false,
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

#[test]
fn a_stack_is_its_shown_leaf_with_the_stack_after_it() {
    let entry = |byte| EntryRef {
        entry: [byte; 16],
        target: EntryTarget::Member(ResourceKey::new([byte; 16]).expect("non-zero key")),
    };
    let doc = |tree| ArrangementDoc {
        groups: vec![ArrangementGroup::new([9; 16], None, tree)],
        stacks: true,
    };
    let stacked = doc(ArrangementNode::Stack {
        active: 1,
        entries: vec![entry(1), entry(2)],
    });
    let bytes = encoded(&stacked);
    assert_eq!(decode_arrangement_doc(&bytes), Ok(stacked));

    // What an older peer reads: the leaf record grown by the stack, which
    // it skips, so it sees the shown entry.
    let shown = doc(ArrangementNode::Leaf(entry(2)));
    let (group, node) = (2, 2 + 4 + 16 + 1);
    let mut trailer = vec![0x01, 0x00, 0x01, 0x00, 0x02];
    for byte in [1, 2] {
        trailer.extend([byte; 16]);
        trailer.push(0x00);
        trailer.extend([byte; 16]);
    }
    assert_eq!(bytes, grow(&encoded(&shown), node, Some(group), &trailer));

    // A stack whose shown entry isn't the leaf in front of it is refused.
    // `active`'s low byte: before the count and two 33-byte entries, and
    // the document's trailing flag.
    let mut lying = bytes.clone();
    lying[bytes.len() - 1 - 2 * 33 - 2 - 1] = 0;
    assert_eq!(
        decode_arrangement_doc(&lying),
        Err(DecodeError::InvalidArrangement)
    );
}
