//! The live replay sends the outer terminal only what it lacks.

use super::*;

#[test]
fn unchanged_images_are_never_retransmitted_and_moves_send_only_placements() {
    let mut source = terminal();
    transmit_pixels(&mut source, 7, 32, 8);
    source.vt_write(b"\x1b[2;2H\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    let mut residency = KittyResidency::default();
    let mut target = terminal();

    let first = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(contains(&first, b"a=t,"), 1);
    target.vt_write(&first);
    assert_eq!(placements(&target), placements(&source));

    // A clean repaint, and a keystroke away from the image, cost nothing.
    assert!(replay_with(&source, &mut residency, (0, 0)).is_empty());
    source.vt_write(b"\x1b[5;1Hhello");
    assert!(replay_with(&source, &mut residency, (0, 0)).is_empty());

    // Scrolling moves the placement: re-place it, keep the pixels.
    source.vt_write(b"\n");
    let scrolled = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(contains(&scrolled, b"a=t,"), 0);
    assert_eq!(contains(&scrolled, b"a=d,d=i,i=7,"), 1);
    assert_eq!(contains(&scrolled, b"a=p,"), 1);
    target.vt_write(&scrolled);
    assert_eq!(placements(&target), placements(&source));
    assert!(replay_with(&source, &mut residency, (0, 0)).is_empty());

    // So does a moved origin.
    let moved = replay_with(&source, &mut residency, (2, 1));
    assert_eq!(contains(&moved, b"a=t,"), 0);
    assert_eq!(contains(&moved, b"a=p,"), 1);
    target.vt_write(&moved);
    assert_eq!(placements(&target).len(), 1);
    assert_eq!(pixels(&target, 7), pixels(&source, 7));
}

#[test]
fn a_changed_image_is_transmitted_once() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\");
    let mut residency = KittyResidency::default();
    let mut target = terminal();
    target.vt_write(&replay_with(&source, &mut residency, (0, 0)));

    // The same id retransmitted with new pixels (which drops its placements)
    // and placed again where it was.
    transmit_pixels(&mut source, 7, 32, 8);
    source.vt_write(b"\x1b[1;1H\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\");
    let changed = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(contains(&changed, b"a=t,"), 1);
    target.vt_write(&changed);
    assert_eq!(pixels(&target, 7), pixels(&source, 7));
    assert_eq!(placements(&target), placements(&source));
    assert!(replay_with(&source, &mut residency, (0, 0)).is_empty());
}

#[test]
fn images_that_leave_the_pane_are_deleted_from_the_outer_terminal() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    transmit(&mut source, 8, 8, 8);
    source.vt_write(
        b"\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\\x1b_Ga=p,i=8,p=1,U=1,c=1,r=1,q=2;\x1b\\",
    );
    let mut residency = KittyResidency::default();
    let mut target = terminal();
    target.vt_write(&replay_with(&source, &mut residency, (0, 0)));
    assert!(pixels(&target, 7).is_some() && pixels(&target, 8).is_some());

    source.vt_write(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\\x1b_Ga=d,d=I,i=8,q=2;\x1b\\");
    let mut iter = PlacementIterator::new().unwrap();
    let mut out = Vec::new();
    // Removing a classic placement reports that cells were touched.
    assert!(
        emit_kitty_graphics_replay(
            &source,
            &mut iter,
            &mut residency,
            &mut out,
            (0, 0),
            (10, 5)
        )
        .unwrap()
    );
    assert_eq!(contains(&out, b"a=d,d=I,i=7,"), 1);
    assert_eq!(contains(&out, b"a=d,d=I,i=8,"), 1);
    target.vt_write(&out);
    assert!(pixels(&target, 7).is_none() && pixels(&target, 8).is_none());
    assert!(replay_with(&source, &mut residency, (0, 0)).is_empty());
}

#[test]
fn placeholder_only_replays_never_touch_cells() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,p=3,U=1,c=2,r=2,q=2;\x1b\\");
    let mut iter = PlacementIterator::new().unwrap();
    let mut residency = KittyResidency::default();
    let mut out = Vec::new();
    assert!(
        !emit_kitty_graphics_replay(
            &source,
            &mut iter,
            &mut residency,
            &mut out,
            (0, 0),
            (10, 5)
        )
        .unwrap()
    );
    assert_eq!(contains(&out, b"a=t,"), 1);
    assert!(!out.windows(2).any(|w| w == b"\x1b["), "no cursor motion");
}

#[test]
fn forget_retransmits_without_duplicating_anonymous_placements() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b[2;2H\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    let mut residency = KittyResidency::default();
    let mut target = terminal();
    target.vt_write(&replay_with(&source, &mut residency, (0, 0)));

    residency.forget();
    let again = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(contains(&again, b"a=t,"), 1);
    target.vt_write(&again);
    assert_eq!(placements(&target), placements(&source));

    // A forgotten image that has since vanished is still deleted.
    residency.forget();
    source.vt_write(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
    let gone = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(contains(&gone, b"a=d,d=I,i=7,"), 1);
    target.vt_write(&gone);
    assert!(pixels(&target, 7).is_none());
}
