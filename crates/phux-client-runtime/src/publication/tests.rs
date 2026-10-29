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
