//! A heal re-sends what the outer terminal may have dropped, at known cost.

use super::*;

fn sends(residency: &KittyResidency) -> Vec<ImageSend> {
    residency.sends().collect()
}

#[test]
fn sends_report_each_transmission_and_a_content_change_refreshes_it() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\");
    let mut residency = KittyResidency::shared();
    let first = replay_with(&source, &mut residency, (0, 0));
    let [send] = sends(&residency)[..] else {
        panic!("one image held");
    };
    assert_eq!(send.pane_id, 7);
    assert!(send.bytes > 0 && send.bytes < first.len());

    // Moving it keeps the send; new pixels refresh it.
    source.vt_write(b"\n");
    let _ = replay_with(&source, &mut residency, (0, 0));
    assert_eq!(sends(&residency), vec![send]);
    transmit_pixels(&mut source, 7, 32, 8);
    source.vt_write(b"\x1b[1;1H\x1b_Ga=p,i=7,p=1,c=1,r=1,C=1,q=2;\x1b\\");
    let _ = replay_with(&source, &mut residency, (0, 0));
    let [changed] = sends(&residency)[..] else {
        panic!("one image held");
    };
    assert!(changed.sent > send.sent);
    assert_ne!(changed.bytes, send.bytes);
}

/// Re-sending an image shown only through Unicode placeholders writes its
/// payload and placement but reports no touched cells, so the pane's front
/// survives; a classic image's re-send does touch them.
#[test]
fn a_placeholder_only_resend_leaves_cells_alone() {
    let mut source = terminal();
    transmit(&mut source, 7, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=7,p=3,U=1,c=2,r=2,q=2;\x1b\\");
    let mut residency = KittyResidency::shared();
    let _ = replay_with(&source, &mut residency, (0, 0));

    for resend in [
        (|r: &mut KittyResidency| r.resend(7)) as fn(&mut KittyResidency),
        KittyResidency::resend_all,
    ] {
        resend(&mut residency);
        assert!(sends(&residency).is_empty(), "owed, not held");
        let mut iter = PlacementIterator::new().unwrap();
        let mut out = Vec::new();
        let touched = emit_kitty_graphics_replay(
            &source,
            &mut iter,
            &mut residency,
            &mut out,
            (0, 0),
            (10, 5),
        )
        .unwrap();
        assert!(!touched);
        assert_eq!(contains(&out, b"a=t,"), 1);
        assert_eq!(contains(&out, b"a=p,"), 1);
        assert!(!out.windows(2).any(|w| w == b"\x1b["), "no cursor motion");
    }

    transmit(&mut source, 8, 8, 8);
    source.vt_write(b"\x1b_Ga=p,i=8,c=1,r=1,C=1,q=2;\x1b\\");
    let _ = replay_with(&source, &mut residency, (0, 0));
    residency.resend(8);
    let mut iter = PlacementIterator::new().unwrap();
    let mut out = Vec::new();
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
    assert_eq!(contains(&out, b"a=t,"), 1, "only the owed image");
}
