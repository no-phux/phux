use super::*;
fn frame(id: &ResourceId, text: &[u8]) -> GridFrame {
    let mut buffer = GridBuffer::default();
    buffer.utf8.extend_from_slice(text);
    for (index, _) in text.iter().enumerate() {
        buffer.cells.push(Cell {
            utf8_offset: u32::try_from(index).expect("small"),
            utf8_len: 1,
            ..Cell::default()
        });
        buffer.metadata.push(CellMetadata::default());
    }
    buffer.row_dirty.push(true);
    GridFrame {
        terminal_id: id.clone(),
        generation: 0,
        stream_id: 1,
        bootstrap_id: 1,
        last_seq: 0,
        cols: u16::try_from(text.len()).expect("small"),
        rows: 1,
        cursor: Cursor::default(),
        scrollbar: Scrollbar::default(),
        colors: FrameColors::default(),
        images: Vec::new(),
        damage: GridDamage::Full,
        buffer,
    }
}

#[test]
fn generations_advance_per_publish_and_held_frames_survive() {
    let table = Publication::new();
    let id = ResourceId::local(3);
    assert!(table.acquire(&id).is_none());
    assert_eq!(table.generation(&id), None);

    assert!(table.publish(&id, frame(&id, b"ab")).is_none());
    let first = table.acquire(&id).expect("published");
    assert_eq!(first.generation, 1);
    assert_eq!(table.generation(&id), Some(1));
    assert_eq!(first.text(), "ab");
    assert_eq!(first.dirty_rows().collect::<Vec<_>>(), vec![0]);

    let handle = table.slot(&id).expect("slot");
    let replaced = table.publish(&id, frame(&id, b"cd")).expect("replaced");
    assert_eq!(replaced.generation, 1);
    assert_eq!(handle.generation(), 2);
    assert_eq!(first.text(), "ab", "a held frame is never mutated");
    assert_eq!(handle.acquire().expect("current").text(), "cd");

    table.remove(&id);
    assert!(table.acquire(&id).is_none());
    assert_eq!(handle.generation(), 2);
    assert!(handle.acquire().is_none());
    assert_eq!(first.text(), "ab");
}

fn document(id: &ResourceId, seq: u64, bytes: &[u8]) -> Arc<DocumentPublication> {
    Arc::new(DocumentPublication {
        resource_id: id.clone(),
        kind: phux_protocol::ResourceKind::AccessibilityTree,
        seq,
        bytes: bytes.to_vec(),
        content_type: None,
    })
}

#[test]
fn a_document_slot_shares_the_generation_and_never_reads_as_a_grid() {
    let table = Publication::new();
    let tree = ResourceId::local(4);
    let terminal = ResourceId::local(3);
    assert!(table.acquire_document(&tree).is_none());

    assert_eq!(table.publish_document(document(&tree, 1, b"{}")), 1);
    table.publish(&terminal, frame(&terminal, b"ab"));
    let first = table.acquire_document(&tree).expect("published");
    assert_eq!(table.generation(&tree), Some(1));
    assert!(table.acquire(&tree).is_none(), "a document is not a grid");
    assert!(table.acquire_document(&terminal).is_none());
    assert_eq!(table.terminals(), vec![terminal.clone()]);
    let mut resources = table.resources();
    resources.sort_by_key(ToString::to_string);
    assert_eq!(resources, vec![terminal, tree.clone()]);

    // Newest wins; the one handed out stays whole.
    let handle = table.slot(&tree).expect("slot");
    assert_eq!(table.publish_document(document(&tree, 2, b"{\"a\":1}")), 2);
    assert_eq!(handle.generation(), 2);
    assert!(handle.acquire().is_none(), "the grid handle sees no grid");
    assert_eq!(first.bytes, b"{}");
    assert!(matches!(
        table.acquire_published(&tree),
        Some(Published::Document(document)) if document.seq == 2
    ));
    // A wake reads only its own generation: the stale one finds nothing.
    assert!(table.acquire_document_at(&tree, 1).is_none());
    assert_eq!(table.acquire_document_at(&tree, 2).expect("current").seq, 2);
    assert!(
        table
            .acquire_document_at(&ResourceId::local(3), 1)
            .is_none()
    );
    // A document is never deferred, so an acquire never asks for a catch-up.
    assert!(!table.stale(&FrameKey::Default(tree.clone())));

    table.remove(&tree);
    assert!(table.acquire_document(&tree).is_none());
    assert!(table.acquire_document_at(&tree, 2).is_none());
    assert_eq!(handle.generation(), 2);
    assert_eq!(first.bytes, b"{}");
}

#[test]
fn wait_past_wakes_on_publish_and_removal_and_is_bounded() {
    use std::time::{Duration, Instant};
    let table = Arc::new(Publication::new());
    let id = ResourceId::local(9);
    table.publish(&id, frame(&id, b"a"));
    let handle = table.slot(&id).expect("slot");
    assert!(handle.wait_past(0, Duration::ZERO), "already past");
    let start = Instant::now();
    assert!(!handle.wait_past(1, Duration::from_millis(20)));
    assert!(start.elapsed() >= Duration::from_millis(20));

    for remove in [false, true] {
        let publisher = Arc::clone(&table);
        let key = id.clone();
        let generation = handle.generation();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            if remove {
                publisher.remove(&key);
            } else {
                publisher.publish(&key, frame(&key, b"b"));
            }
        });
        let start = Instant::now();
        assert!(handle.wait_past(generation, Duration::from_secs(10)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "woken, not timed out"
        );
        thread.join().expect("publisher");
    }
}
