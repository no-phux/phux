//! The document reader and target checks.
use super::*;
use phux_protocol::ids::{BootstrapId, StreamId};

#[test]
fn invalid_target_labels_are_refused_before_spawn() {
    let check = |target: &str| super::validate_target(super::Kind::Tree, target);
    assert!(check("").is_err());
    assert!(check(&"x".repeat(121)).is_err());
    assert!(check(&"雪".repeat(41)).is_err());
    assert!(check(&"雪".repeat(40)).is_ok());
    assert!(super::validate_target(super::Kind::Document, "markdown").is_err());
}

#[test]
fn a_bootstrap_folds_into_one_document_and_live_frames_into_one_each() {
    let tree = ResourceId::local(4);
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let mut reader = Reader::new(tree.clone());
    let begin = FrameKind::BootstrapBegin {
        terminal_id: tree.clone(),
        stream_id,
        bootstrap_id,
        profile: BootstrapStreamProfile::AccessibilityTreeJsonV1,
        cols: 0,
        rows: 0,
        base_seq: 7,
    };
    assert!(matches!(reader.absorb(begin).unwrap(), Absorbed::Nothing));
    for (chunk_seq, payload) in [(0, &b"{\"a\":"[..]), (1, &b"1}"[..])] {
        let chunk = FrameKind::BootstrapChunk {
            terminal_id: tree.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: bytes::Bytes::copy_from_slice(payload),
        };
        assert!(matches!(reader.absorb(chunk).unwrap(), Absorbed::Nothing));
    }
    let ready = FrameKind::BootstrapReady {
        terminal_id: tree.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    };
    let Absorbed::Document(document) = reader.absorb(ready).unwrap() else {
        panic!("READY yields the document");
    };
    assert_eq!(
        document,
        Document {
            seq: 7,
            bytes: b"{\"a\":1}".to_vec(),
            content_type: None,
        }
    );
    let live = FrameKind::ResourceOutput {
        terminal_id: tree,
        stream_id,
        bootstrap_id,
        seq: 8,
        bytes: bytes::Bytes::from_static(b"{\"a\":2}"),
    };
    let Absorbed::Document(document) = reader.absorb(live).unwrap() else {
        panic!("live output yields a document");
    };
    assert_eq!(document.seq, 8);
}

#[test]
fn a_typed_bootstrap_names_the_content_type_of_every_later_document() {
    let doc = ResourceId::local(5);
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let mut reader = Reader::new(doc.clone());
    let frames = [
        FrameKind::BootstrapBegin {
            terminal_id: doc.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::DocumentTypedV1,
            cols: 0,
            rows: 0,
            base_seq: 2,
        },
        FrameKind::BootstrapChunk {
            terminal_id: doc.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: phux_protocol::wire::frame::typed_document::encode_bootstrap(
                "text/markdown",
                b"# hi",
            )
            .into(),
        },
    ];
    for frame in frames {
        assert!(matches!(reader.absorb(frame).unwrap(), Absorbed::Nothing));
    }
    let ready = FrameKind::BootstrapReady {
        terminal_id: doc.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    };
    let Absorbed::Document(document) = reader.absorb(ready).unwrap() else {
        panic!("READY yields the document");
    };
    assert_eq!(document.bytes, b"# hi");
    assert_eq!(document.content_type.as_deref(), Some("text/markdown"));
    let live = FrameKind::ResourceOutput {
        terminal_id: doc,
        stream_id,
        bootstrap_id,
        seq: 3,
        bytes: bytes::Bytes::from_static(b"# two"),
    };
    let Absorbed::Document(document) = reader.absorb(live).unwrap() else {
        panic!("live output yields a document");
    };
    assert_eq!(
        (document.seq, document.content_type.as_deref()),
        (3, Some("text/markdown"))
    );
}

#[test]
fn an_old_server_is_told_apart_by_the_kind_it_lacks() {
    assert_eq!(
        TreeError::Unsupported(Some(Kind::Surface)).to_string(),
        "this server does not serve pixel surfaces (needs PIXEL_SURFACE)"
    );
    assert_eq!(
        TreeError::Unsupported(Some(Kind::Tree)).to_string(),
        "this server does not serve accessibility trees (needs ACCESSIBILITY_TREE)"
    );
    assert!(
        TreeError::Unsupported(None)
            .to_string()
            .contains("PIXEL_SURFACE")
    );
}
