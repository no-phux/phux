//! Kitty graphics through the pane renderer onto the shared outer terminal.

use super::*;

/// A 1x1 red RGBA image transmitted and placed at the cursor as `id`.
fn red(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=T,f=32,s=1,v=1,i={id},q=2;/wAA/w==\x1b\\").into_bytes()
}

fn kitty_commands(bytes: &[u8], action: &[u8]) -> usize {
    let needle = [b"\x1b_Ga=".as_slice(), action].concat();
    bytes.windows(needle.len()).filter(|w| *w == needle).count()
}

fn paint_slot(slot: &mut PaneSlot) -> Vec<u8> {
    paint_at(slot, (0, 0))
}

fn paint_at(slot: &mut PaneSlot, origin: (u16, u16)) -> Vec<u8> {
    let mut out = Vec::new();
    let clip = slot.geometry;
    slot.renderer
        .render_at(
            ReplicaWalk::for_test(&slot.terminal),
            &mut out,
            origin,
            clip,
        )
        .expect("render");
    out
}

fn outer(slot: &PaneSlot, pane_id: u32) -> u32 {
    slot.renderer.kitty_outer_id(pane_id).expect("mapped id")
}

fn pixels(slot: &PaneSlot, id: u32) -> Option<Vec<u8>> {
    let graphics = slot.terminal.kitty_graphics().unwrap();
    Some(graphics.image(id)?.data().unwrap()?.to_vec())
}

fn placed_ids(slot: &PaneSlot) -> Vec<(u32, bool)> {
    let graphics = slot.terminal.kitty_graphics().unwrap();
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    let mut ids = Vec::new();
    while let Some(placement) = placements.next() {
        ids.push((
            placement.image_id().unwrap(),
            placement.is_virtual().unwrap(),
        ));
    }
    ids.sort_unstable();
    ids
}

#[test]
fn first_live_kitty_render_restores_a_visible_image() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    slot.terminal.vt_write(&red(77));
    let out = paint_slot(&mut slot);
    let id = outer(&slot, 77);
    let mut receiver = PaneSlot::new_with_size(10, 5).expect("receiver");
    receiver.terminal.vt_write(&out);
    assert_eq!(pixels(&receiver, id).expect("replayed"), [255, 0, 0, 255]);
    let graphics = receiver.terminal.kitty_graphics().unwrap();
    let image = graphics.image(id).expect("replayed image");
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    let placement = placements.next().expect("visible placement");
    let size = placement.pixel_size(&image, &receiver.terminal).unwrap();
    assert_eq!((size.width, size.height), (1, 1));
    assert_eq!(
        placement
            .viewport_pos(&image, &receiver.terminal)
            .unwrap()
            .map(|p| (p.col, p.row)),
        Some((0, 0))
    );
    assert!(placements.next().is_none());
}

/// An image on screen is sent once: clean repaints and keystrokes that
/// repaint the prompt under it carry no image bytes and keep the front
/// buffer, so the keystroke repaints only its own cells.
#[test]
fn kitty_images_are_sent_once_not_on_every_repaint() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    slot.terminal.vt_write(&red(77));
    slot.terminal.vt_write(b"\r\n\r\n$ ");
    let first = paint_slot(&mut slot);
    assert_eq!(kitty_commands(&first, b"t,"), 1);
    assert_eq!(kitty_commands(&first, b"p,"), 1);

    assert!(paint_slot(&mut slot).is_empty(), "a clean frame is silent");
    // Two dirty paints: the first may rewrite its row whole (the image's
    // placement forgot the front), the second must be a cell diff.
    slot.terminal.vt_write(b"l");
    let _ = paint_slot(&mut slot);
    slot.terminal.vt_write(b"s");
    let typed = paint_slot(&mut slot);
    assert!(!typed.windows(3).any(|w| w == b"\x1b_G"), "{typed:?}");
    assert!(
        !String::from_utf8_lossy(&typed).contains("$ l"),
        "{typed:?}"
    );
}

/// A forced paint and an outside invalidation each assume the outer terminal
/// lost its images and re-send them once.
#[test]
fn kitty_residency_resets_on_forced_paint_and_invalidation() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    slot.terminal.vt_write(&red(77));
    let _ = paint_slot(&mut slot);

    let mut forced = Vec::new();
    slot.renderer
        .render_at_full(
            ReplicaWalk::for_test(&slot.terminal),
            &mut forced,
            (0, 0),
            (10, 5),
        )
        .expect("forced paint");
    assert_eq!(kitty_commands(&forced, b"t,"), 1);
    assert!(paint_slot(&mut slot).is_empty());

    slot.renderer.invalidate_front();
    assert_eq!(kitty_commands(&paint_slot(&mut slot), b"t,"), 1);
    assert!(paint_slot(&mut slot).is_empty());
}

/// An image deleted inside the pane is deleted from the outer terminal.
#[test]
fn a_deleted_kitty_image_is_deleted_from_the_outer_terminal() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    slot.terminal.vt_write(&red(77));
    let mut receiver = PaneSlot::new_with_size(10, 5).expect("receiver");
    receiver.terminal.vt_write(&paint_slot(&mut slot));
    let id = outer(&slot, 77);
    assert!(pixels(&receiver, id).is_some());

    slot.terminal.vt_write(b"\x1b_Ga=d,d=I,i=77,q=2;\x1b\\");
    let deleted = paint_slot(&mut slot);
    let delete = format!("d,d=I,i={id},");
    assert_eq!(kitty_commands(&deleted, delete.as_bytes()), 1);
    receiver.terminal.vt_write(&deleted);
    assert!(pixels(&receiver, id).is_none());
}

/// Two panes numbering their images alike each show their own image, and
/// one pane deleting its image leaves the other's on the outer terminal.
#[test]
fn panes_with_colliding_image_ids_each_show_their_own_image() {
    let mut a = PaneSlot::new_with_size(10, 4).expect("pane a");
    let mut b = PaneSlot::new_with_size(10, 4).expect("pane b");
    a.terminal.vt_write(&red(77));
    b.terminal
        .vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;AP8A/w==\x1b\\");
    let mut outer_terminal = PaneSlot::new_with_size(10, 8).expect("outer");
    outer_terminal.terminal.vt_write(&paint_at(&mut a, (0, 0)));
    outer_terminal.terminal.vt_write(&paint_at(&mut b, (0, 4)));

    let (id_a, id_b) = (outer(&a, 77), outer(&b, 77));
    assert_ne!(id_a, id_b);
    assert_eq!(pixels(&outer_terminal, id_a).unwrap(), [255, 0, 0, 255]);
    assert_eq!(pixels(&outer_terminal, id_b).unwrap(), [0, 255, 0, 255]);
    assert_eq!(placed_ids(&outer_terminal).len(), 2);

    b.terminal.vt_write(b"\x1b_Ga=d,d=I,i=77,q=2;\x1b\\");
    outer_terminal.terminal.vt_write(&paint_at(&mut b, (0, 4)));
    assert!(pixels(&outer_terminal, id_b).is_none());
    assert_eq!(pixels(&outer_terminal, id_a).unwrap(), [255, 0, 0, 255]);
    assert_eq!(placed_ids(&outer_terminal), vec![(id_a, false)]);
}

/// A Unicode placeholder in pane B is painted naming B's outer image, never
/// pane A's image with the same pane id.
#[test]
fn a_placeholder_in_pane_b_is_drawn_with_bs_pixels() {
    let mut a = PaneSlot::new_with_size(10, 4).expect("pane a");
    let mut b = PaneSlot::new_with_size(10, 4).expect("pane b");
    a.terminal.vt_write(&red(77));
    // Image 77 with a virtual placement, shown by one placeholder cell whose
    // 256-colour foreground spells the id.
    b.terminal
        .vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=77,U=1,c=1,r=1,q=2;AP8A/w==\x1b\\");
    b.terminal
        .vt_write("\x1b[38;5;77m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
    let mut outer_terminal = PaneSlot::new_with_size(10, 8).expect("outer");
    outer_terminal.terminal.vt_write(&paint_at(&mut a, (0, 0)));
    let painted = paint_at(&mut b, (0, 4));
    outer_terminal.terminal.vt_write(&painted);

    let id_b = outer(&b, 77);
    assert_ne!(id_b, outer(&a, 77));
    let [_, r, g, bl] = id_b.to_be_bytes();
    let text = String::from_utf8_lossy(&painted);
    assert!(text.contains(&format!("38;2;{r};{g};{bl}m")), "{text:?}");
    assert!(!text.contains("38;5;77"), "{text:?}");
    assert_eq!(pixels(&outer_terminal, id_b).unwrap(), [0, 255, 0, 255]);
    assert!(placed_ids(&outer_terminal).contains(&(id_b, true)));

    // The rewritten colour is stable: a clean repaint sends nothing.
    assert!(paint_at(&mut b, (0, 4)).is_empty());
}

/// A pane showing an image is due a heal one interval after the full send;
/// the heal re-sends it once, and a pane without images is never due.
#[test]
fn a_heal_re_sends_visible_images_once_per_interval() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    let _ = paint_slot(&mut slot);
    assert!(slot.renderer.kitty_heal_at().is_none());
    slot.terminal.vt_write(&red(77));
    let before = tokio::time::Instant::now();
    let _ = paint_slot(&mut slot);
    let due = slot.renderer.kitty_heal_at().expect("due a heal");
    assert!(due >= before + TerminalRenderer::KITTY_HEAL_INTERVAL);
    assert!(due <= tokio::time::Instant::now() + TerminalRenderer::KITTY_HEAL_INTERVAL);
    // Clean repaints neither re-send nor push the deadline out.
    assert!(paint_slot(&mut slot).is_empty());
    assert_eq!(slot.renderer.kitty_heal_at(), Some(due));

    let mut panes = HashMap::from([(ResourceId::local(1), slot)]);
    assert_eq!(kitty_heal_deadline(&panes), Some(due));
    heal_kitty(&mut panes);
    let slot = panes.get_mut(&ResourceId::local(1)).expect("slot");
    assert!(slot.renderer.kitty_heal_at().is_none());
    let healed = paint_slot(slot);
    assert_eq!(kitty_commands(&healed, b"t,"), 1, "{healed:?}");
    assert!(
        slot.renderer
            .kitty_heal_at()
            .is_some_and(|again| again >= due)
    );
    assert!(paint_slot(slot).is_empty());

    slot.terminal.vt_write(b"\x1b_Ga=d,d=I,i=77,q=2;\x1b\\");
    let _ = paint_slot(slot);
    assert!(slot.renderer.kitty_heal_at().is_none(), "no image, no heal");
}
