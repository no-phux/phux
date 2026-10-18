use super::*;
use crate::c::error::bytes_in;
use crate::c::phux_client_free;
use crate::c::test_support::{
    attached_frame, attaching, feed, feed_bootstrap, new_client, single_terminal_snapshot,
};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::FrameKind;

/// A client attached to one 20x2 terminal that printed `payload`.
fn client_showing(payload: &[u8]) -> *mut PhuxClient {
    let client = attaching(&[], 1);
    let terminal = ResourceId::local(1);
    assert_eq!(
        feed(
            client,
            &attached_frame(1, single_terminal_snapshot(terminal.clone(), 20, 2))
        ),
        PhuxClientResult::Ok
    );
    feed_bootstrap(client, &terminal, (1, 1), (20, 2), payload);
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 1 }),
        PhuxClientResult::Ok
    );
    client
}

fn terminal() -> PhuxResourceId {
    PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    }
}

/// # Safety
///
/// `list` came from `phux_client_terminal_draw_list` and no mutable client
/// call has happened since.
unsafe fn parts<'a, T>(data: *const T, len: usize) -> &'a [T] {
    if data.is_null() {
        &[]
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { std::slice::from_raw_parts(data, len) }
    }
}

#[test]
fn a_grid_lends_runs_blocks_decorations_and_the_cursor() {
    let client =
        client_showing(b"hi \x1b[1mB\x1b[0m \x1b[41m  \x1b[0m\x1b[4mun\x1b[0m\xe2\x96\x88");
    let terminal = terminal();
    let mut list = PhuxDrawList::default();
    // SAFETY: test-owned client, id, null options and output storage.
    let result = unsafe {
        phux_client_terminal_draw_list(client, &raw const terminal, ptr::null(), &raw mut list)
    };
    assert_eq!(result, PhuxClientResult::Ok);
    assert_eq!((list.cols, list.rows), (20, 2));
    // SAFETY: borrowed views consumed before the next mutable call.
    let (texts, backgrounds, blocks, decorations, utf8) = unsafe {
        (
            parts(list.texts, list.text_count),
            parts(list.backgrounds, list.background_count),
            parts(list.blocks, list.block_count),
            parts(list.decorations, list.decoration_count),
            bytes_in(list.utf8.data, list.utf8.len).expect("utf8"),
        )
    };
    let words: Vec<_> = texts
        .iter()
        .map(|run| {
            let start = run.utf8_offset as usize;
            let text = std::str::from_utf8(&utf8[start..start + run.utf8_len as usize]);
            (run.col, run.cells, text.expect("utf8"), run.face)
        })
        .collect();
    assert_eq!(
        words,
        vec![
            (0, 2, "hi", 0),
            (3, 1, "B", PHUX_DRAW_FACE_BOLD),
            (7, 2, "un", 0),
        ]
    );
    assert_eq!(backgrounds.len(), 1);
    assert_eq!((backgrounds[0].start, backgrounds[0].end), (5, 7));
    assert_eq!(blocks.len(), 1);
    assert_eq!((blocks[0].col, blocks[0].region_count), (9, 1));
    assert_eq!(
        (blocks[0].regions[0].width, blocks[0].regions[0].height),
        (8, 8)
    );
    assert_eq!(decorations.len(), 1);
    assert_eq!(
        (
            decorations[0].start,
            decorations[0].end,
            decorations[0].kind
        ),
        (7, 9, PHUX_DRAW_UNDERLINE_SINGLE)
    );
    assert!(list.cursor.visible);
    assert_eq!((list.cursor.row, list.cursor.col), (0, 10));
    // SAFETY: test-owned client.
    unsafe { phux_client_free(client) };
}

#[test]
fn options_and_output_are_validated() {
    let client = client_showing(b"x");
    let terminal = terminal();
    let mut list = PhuxDrawList::default();
    let stale = PhuxDrawOptions {
        version: ABI_VERSION + 1,
        ..PhuxDrawOptions::default()
    };
    // SAFETY: test-owned pointers throughout.
    unsafe {
        assert_eq!(
            phux_client_terminal_draw_list(
                client,
                &raw const terminal,
                &raw const stale,
                &raw mut list
            ),
            PhuxClientResult::InvalidArgument
        );
        let unfocused = PhuxDrawOptions {
            focused: false,
            ..PhuxDrawOptions::default()
        };
        assert_eq!(
            phux_client_terminal_draw_list(
                client,
                &raw const terminal,
                &raw const unfocused,
                &raw mut list
            ),
            PhuxClientResult::Ok
        );
        assert!(!list.cursor.solid);
        let mut short = PhuxDrawList {
            size: size_of::<usize>(),
            ..PhuxDrawList::default()
        };
        assert_eq!(
            phux_client_terminal_draw_list(
                client,
                &raw const terminal,
                ptr::null(),
                &raw mut short
            ),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            phux_client_terminal_draw_list(
                client,
                &raw const terminal,
                ptr::null(),
                ptr::null_mut()
            ),
            PhuxClientResult::InvalidArgument
        );
        phux_client_free(client);
    }
}

#[test]
fn repeated_draws_hold_no_top_anchor() {
    let client = client_showing(b"x");
    let terminal = terminal();
    let mut list = PhuxDrawList::default();
    for _ in 0..3 {
        // SAFETY: test-owned pointers.
        let result = unsafe {
            phux_client_terminal_draw_list(client, &raw const terminal, ptr::null(), &raw mut list)
        };
        assert_eq!(result, PhuxClientResult::Ok);
        // SAFETY: the live test client, read between calls.
        let render = unsafe { &(*client).inner.render };
        let anchor = render[&ResourceId::local(1)].view.top_anchor;
        assert_eq!(
            anchor.opaque_id, 0,
            "the grid's anchor is released, not lent"
        );
    }
    // SAFETY: test-owned client.
    unsafe { phux_client_free(client) };
}

#[test]
fn layouts_match_the_header() {
    // tests/draw_list_abi.c pins the same numbers against include/phux/client.h.
    assert_eq!(size_of::<PhuxDrawOptions>(), 24);
    assert_eq!(std::mem::offset_of!(PhuxDrawOptions, focused), 18);
    assert_eq!(size_of::<PhuxDrawBackground>(), 10);
    assert_eq!(size_of::<PhuxDrawText>(), 20);
    assert_eq!(std::mem::offset_of!(PhuxDrawText, utf8_offset), 12);
    assert_eq!(size_of::<PhuxDrawBlock>(), 18);
    assert_eq!(std::mem::offset_of!(PhuxDrawBlock, regions), 9);
    assert_eq!(size_of::<PhuxDrawDecoration>(), 12);
    assert_eq!(size_of::<PhuxDrawCursor>(), 16);
    assert_eq!(std::mem::offset_of!(PhuxDrawCursor, style), 8);
    assert_eq!(size_of::<PhuxDrawList>(), 120);
    assert_eq!(std::mem::offset_of!(PhuxDrawList, backgrounds), 24);
    assert_eq!(std::mem::offset_of!(PhuxDrawList, cursor), 88);
    assert_eq!(std::mem::offset_of!(PhuxDrawList, utf8), 104);
}

#[test]
fn a_terminal_without_a_grid_is_invalid_state() {
    let client = new_client();
    let terminal = terminal();
    let mut list = PhuxDrawList::default();
    // SAFETY: test-owned pointers.
    let result = unsafe {
        phux_client_terminal_draw_list(client, &raw const terminal, ptr::null(), &raw mut list)
    };
    assert_ne!(result, PhuxClientResult::Ok);
    // SAFETY: test-owned client.
    unsafe { phux_client_free(client) };
}
