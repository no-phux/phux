//! Snapshot payloads travel zlib-compressed when that is smaller.

use super::*;

#[test]
fn snapshot_compresses_raw_payloads_and_round_trips_pixels() {
    let mut source = terminal();
    transmit_pixels(&mut source, 7, 32, 64);
    transmit_pixels(&mut source, 8, 24, 64);
    source.vt_write(b"\x1b_Ga=p,i=7,p=1,c=2,r=2,C=1,q=2;\x1b\\");
    let mut iter = PlacementIterator::new().unwrap();
    let mut out = Vec::new();
    assert!(emit_kitty_graphics_snapshot(&source, &mut iter, &mut out).unwrap());
    assert_eq!(contains(&out, b"o=z,"), 2);
    let raw = (64 * 64 * 4 + 64 * 64 * 3) * 4 / 3;
    assert!(out.len() * 4 < raw, "{} bytes against {raw} raw", out.len());

    let mut target = terminal();
    target.vt_write(&out);
    for id in [7, 8] {
        assert_eq!(pixels(&target, id), pixels(&source, id), "image {id}");
    }
    assert_eq!(placements(&target), placements(&source));
}

#[test]
fn incompressible_payloads_travel_raw() {
    let mut source = terminal();
    // One pixel: deflate's framing outweighs four bytes.
    source.vt_write(b"\x1b_Ga=t,f=32,s=1,v=1,i=7,q=2;AAECAw==\x1b\\");
    let mut iter = PlacementIterator::new().unwrap();
    let mut out = Vec::new();
    assert!(emit_kitty_graphics_snapshot(&source, &mut iter, &mut out).unwrap());
    assert_eq!(contains(&out, b"o=z"), 0);
    let mut target = terminal();
    target.vt_write(&out);
    assert_eq!(pixels(&target, 7), Some(vec![0, 1, 2, 3]));
}
