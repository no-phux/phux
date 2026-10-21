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

    // Inside the group record, which starts after the `u16` count.
    let group = 2;
    let grown_group = grow(&bytes, group, None, &[0xEE; 3]);
    assert_eq!(decode_arrangement_doc(&grown_group), Ok(doc.clone()));

    // Inside the split node, after the group's length, id, and name
    // (presence byte, `u32` length, "tab").
    let node = group + 4 + 16 + 1 + 4 + 3;
    let grown_node = grow(&bytes, node, Some(group), &[0xEE; 2]);
    assert_eq!(decode_arrangement_doc(&grown_node), Ok(doc));
}
