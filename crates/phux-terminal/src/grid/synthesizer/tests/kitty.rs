use super::*;

#[test]
fn snapshot_replays_kitty_graphics_placements() {
    let mut terminal = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)
        .expect("kitty config");
    terminal.resize(10, 5, 8, 16).expect("cell geometry");
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,q=2;AAECAw==\x1b\\");

    let snap = synthesize(&terminal).expect("synth");
    let mut replica = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut replica)
        .expect("replica kitty config");
    replica.resize(10, 5, 8, 16).expect("replica cell geometry");
    replica.vt_write(&snap.bytes);
    let graphics = replica.kitty_graphics().expect("graphics");
    let image = graphics.image(9).expect("snapshot image");
    assert_eq!(image.data().unwrap().unwrap(), &[0, 1, 2, 3]);
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    assert!(placements.next().is_some());
    assert!(placements.next().is_none());
}

#[test]
fn repeated_snapshots_remove_deleted_classic_and_virtual_images() {
    let mut source = fresh(10, 5);
    let mut replica = fresh(10, 5);
    for terminal in [&mut source, &mut replica] {
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(terminal)
            .expect("kitty config");
        terminal.resize(10, 5, 8, 16).expect("cell geometry");
    }
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,p=1,C=1,q=2;AAECAw==\x1b\\\x1b_Ga=p,i=9,p=2,U=1,c=2,r=2,q=2;\x1b\\");
    let first = synthesize(&source).expect("first snapshot");
    replica.vt_write(&first.bytes);
    assert!(replica.kitty_graphics().unwrap().image(9).is_some());
    source.vt_write(b"\x1b_Ga=d,d=I,i=9,q=2;\x1b\\");
    let second = synthesize(&source).expect("second snapshot");
    replica.vt_write(&second.bytes);
    assert!(replica.kitty_graphics().unwrap().image(9).is_none());
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let graphics = replica.kitty_graphics().unwrap();
    assert!(iter.update(&graphics).unwrap().next().is_none());
}

#[test]
fn image_replay_preserves_snapshot_cursor() {
    let mut source = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(10, 5, 8, 16).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,p=1,C=1,q=2;AAECAw==\x1b\\\x1b[4;6H");
    let snapshot = synthesize(&source).unwrap();
    let mut replica = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut replica).unwrap();
    replica.resize(10, 5, 8, 16).unwrap();
    replica.vt_write(&snapshot.bytes);
    // A printable byte observes the receiver's actual restored cursor.
    source.vt_write(b"X");
    replica.vt_write(b"X");
    let mut synth = SnapshotSynthesizer::new().unwrap();
    assert_eq!(
        synth.screen_state(&replica, 0).unwrap().lines,
        synth.screen_state(&source, 0).unwrap().lines,
    );
}

#[test]
fn image_payload_obeys_the_synthesis_byte_ceiling() {
    let mut source = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(10, 5, 8, 16).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,p=1,C=1,q=2;AAECAw==\x1b\\");
    let required = synthesize(&source).unwrap().bytes.len();
    assert!(matches!(
        SnapshotSynthesizer::synthesize_bounded(
            &source,
            required - 1,
            phux_protocol::caps::ImageProtocolSet::all()
        ),
        Err(SynthesisError::LimitExceeded)
    ));
}

#[test]
fn text_only_snapshot_omits_unplaced_images_before_charging_budget() {
    let mut source = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(10, 5, 8, 16).unwrap();
    source.vt_write(b"visible text");
    let text_budget = synthesize(&source).unwrap().bytes.len() + 128;
    let payload = "AAAA".repeat(256);
    source.vt_write(format!("\x1b_Ga=t,f=32,s=16,v=12,i=9,q=2;{payload}\x1b\\").as_bytes());
    let graphics = source.kitty_graphics().unwrap();
    assert_eq!(
        graphics.image(9).unwrap().data().unwrap().unwrap().len(),
        768
    );
    let mut placements = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    assert!(placements.update(&graphics).unwrap().next().is_none());
    assert!(matches!(
        SnapshotSynthesizer::synthesize_bounded(
            &source,
            text_budget,
            phux_protocol::caps::ImageProtocolSet::all()
        ),
        Err(SynthesisError::LimitExceeded)
    ));
    let full = synthesize(&source).expect("full image snapshot");
    let mut image_receiver = fresh(10, 5);
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut image_receiver)
        .unwrap();
    image_receiver.vt_write(&full.bytes);
    assert_eq!(
        image_receiver
            .kitty_graphics()
            .unwrap()
            .image(9)
            .unwrap()
            .data()
            .unwrap()
            .unwrap(),
        &[0; 768],
    );
    let snapshot = SnapshotSynthesizer::synthesize_bounded(
        &source,
        text_budget,
        phux_protocol::caps::ImageProtocolSet::new(),
    )
    .expect("unsupported images must not consume the text snapshot budget");
    let mut replica = fresh(10, 5);
    replica.vt_write(&snapshot.bytes);
    let mut synth = SnapshotSynthesizer::new().unwrap();
    assert_eq!(
        synth.screen_state(&replica, 0).unwrap().lines,
        synth.screen_state(&source, 0).unwrap().lines,
    );
}
