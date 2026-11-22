use super::*;
use phux_client_core::grid::GridProjector;

fn terminal() -> Terminal<'static, 'static> {
    let mut terminal = Terminal::new(8, 3).unwrap();
    phux_vt::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    terminal.resize(8, 3, 8, 16).unwrap();
    terminal
}

fn project(images: &mut Images, terminal: &Terminal<'static, 'static>) -> Vec<ImagePlacement> {
    let bar = terminal.scrollbar().unwrap();
    let fringe = (bar.offset + bar.len < bar.total)
        .then(|| u32::try_from(bar.offset + bar.len).expect("bounded fixture history"));
    GridProjector::new()
        .unwrap()
        .project_with_fringe_and_snapshot(terminal, 1, fringe, |snapshot| {
            images.project(terminal, snapshot).map_err(Into::into)
        })
        .unwrap()
        .1
}

#[test]
fn chunked_raw_publication_reuses_pixels_and_deletion_replacement_is_immutable() {
    let mut terminal = terminal();
    let mut images = Images::default();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1,m=1;/wAA\x1b\\");
    assert!(project(&mut images, &terminal).is_empty());
    terminal.vt_write(b"\x1b_Gm=0;/w==\x1b\\");
    let first = project(&mut images, &terminal);
    assert_eq!(first.len(), 1);
    assert_eq!(&*first[0].image.rgba, &[255, 0, 0, 255]);
    assert_eq!((first[0].cols, first[0].rows), (2.0, 2.0));
    let second = project(&mut images, &terminal);
    assert!(Arc::ptr_eq(&first[0].image, &second[0].image));
    terminal.vt_write(b"\x1b_Ga=d,d=I,i=42\x1b\\");
    assert!(project(&mut images, &terminal).is_empty());
    assert!(images.pixels.is_empty());
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1;AAD//w==\x1b\\");
    let replacement = project(&mut images, &terminal);
    assert!(!Arc::ptr_eq(&first[0].image, &replacement[0].image));
    assert_eq!(&*first[0].image.rgba, &[255, 0, 0, 255]);
    assert_eq!(&*replacement[0].image.rgba, &[0, 0, 255, 255]);
}

#[test]
fn chunked_png_is_decoded_once() {
    let mut terminal = terminal();
    let mut images = Images::default();
    terminal.vt_write(
        b"\x1b_Ga=T,f=100,i=7,c=1,r=1,C=1,m=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ\x1b\\",
    );
    terminal.vt_write(b"\x1b_Gm=0;AAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\");
    let first = project(&mut images, &terminal);
    assert_eq!(first.len(), 1);
    assert_eq!((first[0].image.width, first[0].image.height), (1, 1));
    assert_eq!(&*first[0].image.rgba, &[255, 0, 0, 255]);
    assert!(Arc::ptr_eq(
        &first[0].image,
        &project(&mut images, &terminal)[0].image
    ));
}

#[test]
fn scrollback_crop_offsets_and_screen_switch_use_active_geometry() {
    let mut terminal = terminal();
    let mut images = Images::default();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=3,C=1,X=2,Y=4;/wAA/w==\x1b\\");
    let first = project(&mut images, &terminal);
    assert_eq!((first[0].col, first[0].row), (0.25, 0.25));
    terminal.vt_write(b"\r\n\r\n\r\n");
    let scrolled = project(&mut images, &terminal);
    assert_eq!(scrolled[0].row, -0.75);
    terminal.vt_write(b"\x1b[?1049h");
    assert!(project(&mut images, &terminal).is_empty());
    terminal.vt_write(b"\x1b[?1049l");
    assert_eq!(project(&mut images, &terminal)[0].row, -0.75);
    terminal.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Delta(-1));
    assert_eq!(project(&mut images, &terminal)[0].row, 0.25);
}

#[test]
fn images_are_published_in_engine_layer_then_z_order() {
    let mut terminal = terminal();
    terminal.vt_write(b"\x1b_Ga=t,f=32,s=1,v=1,i=42;/wAA/w==\x1b\\");
    for (id, z) in [(4, 5), (3, -1), (2, -1_073_741_825), (1, -2)] {
        terminal.vt_write(format!("\x1b_Ga=p,i=42,p={id},c=1,r=1,C=1,z={z};\x1b\\").as_bytes());
    }
    let placements = project(&mut Images::default(), &terminal);
    assert_eq!(
        placements
            .iter()
            .map(|placement| placement.z)
            .collect::<Vec<_>>(),
        [-1_073_741_825, -2, -1, 5]
    );
}

#[test]
fn virtual_filtering_keeps_the_prototypes_nonzero_source_crop() {
    let mut terminal = terminal();
    terminal.vt_write(
        format!(
            "\x1b_Ga=T,f=24,s=8,v=4,i=42,p=9,U=1,c=2,r=1,x=2,y=1,w=4,h=2;{}\x1b\\",
            "/".repeat(128),
        )
        .as_bytes(),
    );
    terminal.vt_write("\x1b[38;5;42;58;5;9m\u{10eeee}\u{305}\u{30d}".as_bytes());
    let placements = project(&mut Images::default(), &terminal);
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].sample_source, [2.0, 1.0, 4.0, 2.0]);
}
