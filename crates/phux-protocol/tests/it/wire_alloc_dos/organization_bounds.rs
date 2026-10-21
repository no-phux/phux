use phux_protocol::ids::{ArrangementKey, IdempotencyKey, ResourceKey, Revision, SessionKey};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, Command, EntryRef, EntryTarget, FrameKind,
    MAX_ARRANGEMENT_BYTES, MAX_ORG_ATTRS, MAX_ORG_BATCH, MAX_ORG_CURSOR_BYTES, MAX_ORG_NAME_BYTES,
    MoveItem, OrgFilter, RecordRevision, SessionLifetime,
};

use super::{ALLOC_CEILING, largest_alloc_during};

fn op() -> IdempotencyKey {
    IdempotencyKey::new([1; 16]).unwrap()
}

fn session() -> SessionKey {
    SessionKey::new([2; 16]).unwrap()
}

fn frame(command: Command) -> Vec<u8> {
    let mut out = bytes::BytesMut::new();
    FrameKind::Command {
        request_id: 1,
        command,
    }
    .encode(&mut out);
    out.to_vec()
}

fn decodes(command: Command) -> Result<(), DecodeError> {
    FrameKind::decode(&frame(command)).map(drop)
}

fn create(name: String, attrs: usize) -> Command {
    Command::SessionCreate {
        operation: op(),
        name,
        lifetime: SessionLifetime::Persistent,
        attrs: (0..attrs)
            .map(|i| (format!("k{i}"), "v".to_owned()))
            .collect(),
        launch: None,
    }
}

fn leaf(i: u32) -> ArrangementNode {
    let mut key = [7; 16];
    key[..4].copy_from_slice(&i.to_be_bytes());
    ArrangementNode::Leaf(EntryRef {
        entry: key,
        target: EntryTarget::Member(ResourceKey::new(key).unwrap()),
    })
}

fn replace(groups: u32) -> Command {
    Command::ArrangementReplace {
        operation: op(),
        arrangement: ArrangementKey::new([3; 16]).unwrap(),
        expected: Revision::new(1),
        document: ArrangementDoc {
            groups: (0..groups)
                .map(|i| ArrangementGroup {
                    id: [9; 16],
                    name: None,
                    tree: leaf(i),
                })
                .collect(),
        },
    }
}

#[test]
fn names_and_attribute_lists_are_bounded() {
    assert_eq!(decodes(create("a".repeat(MAX_ORG_NAME_BYTES), 0)), Ok(()));
    assert_eq!(
        decodes(create("a".repeat(MAX_ORG_NAME_BYTES + 1), 0)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
    assert_eq!(decodes(create("w".to_owned(), MAX_ORG_ATTRS)), Ok(()));
    assert_eq!(
        decodes(create("w".to_owned(), MAX_ORG_ATTRS + 1)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
}

#[test]
fn arrangement_documents_are_bounded_at_64_kib() {
    // One leaf group is a record: 4 + 16 + 1, then a leaf node record
    // 4 + 1 + 16 + 1 + 16, for 59 bytes.
    let fits = u32::try_from((MAX_ARRANGEMENT_BYTES - 2) / 59).unwrap();
    assert_eq!(decodes(replace(fits)), Ok(()));
    assert_eq!(
        decodes(replace(fits + 1)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
}

#[test]
fn batches_and_cursors_are_bounded() {
    let items = |n: usize| Command::ResourceMoveMany {
        operation: op(),
        items: (0..n)
            .map(|_| MoveItem {
                resource: ResourceKey::new([4; 16]).unwrap(),
                from: session(),
                from_expected: Revision::new(1),
            })
            .collect(),
        to: SessionKey::new([5; 16]).unwrap(),
        to_expected: Revision::new(1),
    };
    assert_eq!(decodes(items(MAX_ORG_BATCH)), Ok(()));
    assert_eq!(
        decodes(items(MAX_ORG_BATCH + 1)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
    let undo = |n: usize| Command::OrganizationUndo {
        operation: op(),
        undo: IdempotencyKey::new([6; 16]).unwrap(),
        expected: (0..n)
            .map(|_| RecordRevision {
                key: [8; 16],
                revision: Revision::new(1),
            })
            .collect(),
    };
    assert_eq!(
        decodes(undo(MAX_ORG_BATCH + 1)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
    let page = |n: usize| Command::GetOrganization {
        filter: OrgFilter::Intake,
        page_size: 10,
        cursor: Some(vec![0; n]),
    };
    assert_eq!(decodes(page(MAX_ORG_CURSOR_BYTES)), Ok(()));
    assert_eq!(
        decodes(page(MAX_ORG_CURSOR_BYTES + 1)),
        Err(DecodeError::OrganizationLimitExceeded)
    );
}

#[test]
fn hostile_group_count_does_not_over_reserve() {
    let mut bytes = frame(replace(1));
    // The document is the last field: rewrite its u16 group count, which
    // sits 2 bytes after the document field's length prefix, to u16::MAX
    // by locating the single group's id run.
    let id_at = bytes
        .windows(16)
        .position(|w| w == [9; 16])
        .expect("the group id is in the frame");
    bytes[id_at - 2..id_at].copy_from_slice(&u16::MAX.to_be_bytes());
    let max = largest_alloc_during(&bytes);
    assert!(FrameKind::decode(&bytes).is_err());
    assert!(max < ALLOC_CEILING, "reserved {max} bytes");
}
