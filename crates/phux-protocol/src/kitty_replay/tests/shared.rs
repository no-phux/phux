//! Panes sharing one outer terminal keep their images apart.

use libghostty_vt::style::{PaletteIndex, StyleColor};

use super::*;

fn outer(residency: &KittyResidency, pane_id: u32) -> u32 {
    residency.outer_id(pane_id).expect("mapped id")
}

#[test]
fn colliding_pane_ids_land_on_distinct_outer_images() {
    let (mut a, mut b) = (terminal(), terminal());
    transmit(&mut a, 7, 8, 8);
    transmit_pixels(&mut b, 7, 32, 8);
    a.vt_write(b"\x1b[1;1H\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    b.vt_write(b"\x1b[3;3H\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    let (mut held_a, mut held_b) = (KittyResidency::shared(), KittyResidency::shared());
    let mut target = terminal();
    target.vt_write(&replay_with(&a, &mut held_a, (0, 0)));
    target.vt_write(&replay_with(&b, &mut held_b, (0, 0)));

    let (id_a, id_b) = (outer(&held_a, 7), outer(&held_b, 7));
    assert_ne!(id_a, id_b);
    assert_eq!(pixels(&target, id_a), pixels(&a, 7));
    assert_eq!(pixels(&target, id_b), pixels(&b, 7));
    assert_eq!(placements(&target).len(), 2);

    // B deleting its image leaves A's alone, and the mapping is stable.
    b.vt_write(b"\x1b_Ga=d,d=I,i=7,q=2;\x1b\\");
    let gone = replay_with(&b, &mut held_b, (0, 0));
    assert_eq!(contains(&gone, format!("a=d,d=I,i={id_b},").as_bytes()), 1);
    target.vt_write(&gone);
    assert!(pixels(&target, id_b).is_none());
    assert_eq!(pixels(&target, id_a), pixels(&a, 7));
    held_a.forget();
    target.vt_write(&replay_with(&a, &mut held_a, (0, 0)));
    assert_eq!(outer(&held_a, 7), id_a);
    assert_eq!(placements(&target).len(), 1);
}

#[test]
fn a_placeholder_colour_names_its_own_panes_outer_image() {
    let (mut a, mut b) = (terminal(), terminal());
    transmit(&mut a, 7, 8, 8);
    transmit_pixels(&mut b, 7, 32, 8);
    for pane in [&mut a, &mut b] {
        pane.vt_write(b"\x1b_Ga=p,i=7,p=2,U=1,c=1,r=1,q=2;\x1b\\");
    }
    let (mut held_a, mut held_b) = (KittyResidency::shared(), KittyResidency::shared());
    let mut target = terminal();
    target.vt_write(&replay_with(&a, &mut held_a, (0, 0)));
    target.vt_write(&replay_with(&b, &mut held_b, (0, 0)));

    // Pane B's placeholder spells image 7 in palette colour 7.
    let fg = held_b.placeholder_fg(StyleColor::Palette(PaletteIndex(7)));
    let named = u32::from_be_bytes([0, fg.r, fg.g, fg.b]);
    assert_eq!(named, outer(&held_b, 7));
    assert_ne!(named, outer(&held_a, 7));
    assert_eq!(pixels(&target, named), pixels(&b, 7));
    assert!(
        placements(&target)
            .iter()
            .any(|p| p.0 == named && p.1 == 2 && p.2),
        "B's virtual placement is on B's outer image"
    );
    // The same colour spelled as RGB maps the same way.
    let rgb = held_b.placeholder_fg(StyleColor::Rgb(libghostty_vt::style::RgbColor {
        r: 0,
        g: 0,
        b: 7,
    }));
    assert_eq!(rgb, fg);
}

#[test]
fn high_byte_ids_keep_their_high_byte() {
    let mut source = terminal();
    let id = 0x7FFF_FFFF;
    transmit(&mut source, id, 8, 8);
    source.vt_write(format!("\x1b_Ga=p,i={id},c=1,r=1,C=1,q=2;\x1b\\").as_bytes());
    let mut held = KittyResidency::shared();
    let mut target = terminal();
    target.vt_write(&replay_with(&source, &mut held, (0, 0)));
    let mapped = outer(&held, id);
    assert_eq!(mapped >> 24, 0x7F);
    assert_eq!(pixels(&target, mapped), pixels(&source, id));
}

#[test]
fn release_and_drop_delete_what_the_outer_terminal_holds() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    let mut held = KittyResidency::shared();
    let mut target = terminal();
    target.vt_write(&replay_with(&source, &mut held, (0, 0)));
    let id = outer(&held, 7);

    let mut out = Vec::new();
    assert!(held.release(&mut out).unwrap(), "a classic placement went");
    assert_eq!(contains(&out, format!("a=d,d=I,i={id},").as_bytes()), 1);
    target.vt_write(&out);
    assert!(pixels(&target, id).is_none());
    out.clear();
    assert!(!held.release(&mut out).unwrap());
    assert!(out.is_empty());

    // Shown again, then dropped while held: the next frame deletes it.
    target.vt_write(&replay_with(&source, &mut held, (0, 0)));
    assert_eq!(outer(&held, 7), id);
    drop(held);
    assert!(write_orphaned_deletes(&mut out).unwrap());
    assert_eq!(contains(&out, format!("a=d,d=I,i={id},").as_bytes()), 1);
    out.clear();
    assert!(!write_orphaned_deletes(&mut out).unwrap());
    assert!(out.is_empty());
}

#[test]
fn unused_mappings_are_pruned_past_the_bound() {
    let mut source = terminal();
    let mut held = KittyResidency::shared();
    // Placeholder cells may name ids that never existed; each gets a mapping.
    for low in 1..=300 {
        let _ = held.placeholder_fg(StyleColor::Rgb(libghostty_vt::style::RgbColor {
            r: 0,
            g: 1,
            b: u8::try_from(low % 256).unwrap(),
        }));
    }
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,c=1,r=1,C=1,q=2;\x1b\\");
    let _ = replay_with(&source, &mut held, (0, 0));
    let kept = outer(&held, 7);
    assert!(
        held.outer_id(0x0001_0005).is_none(),
        "an unused id was pruned"
    );
    let _ = replay_with(&source, &mut held, (0, 0));
    assert_eq!(outer(&held, 7), kept);
}
