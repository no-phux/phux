use super::*;
use phux_client_core::grid::GridProjector;

fn terminal() -> Terminal<'static, 'static> {
    let mut terminal = Terminal::new(8, 3).unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    terminal.resize(8, 3, 8, 16).unwrap();
    terminal
}

fn project(images: &mut Images, terminal: &Terminal<'static, 'static>) -> Vec<ImagePlacement> {
    project_cached(images, &mut GridProjector::new().unwrap(), terminal)
}

fn project_cached(
    images: &mut Images,
    projector: &mut GridProjector,
    terminal: &Terminal<'static, 'static>,
) -> Vec<ImagePlacement> {
    let bar = terminal.scrollbar().unwrap();
    let fringe = (bar.offset + bar.len < bar.total)
        .then(|| u32::try_from(bar.offset + bar.len).expect("bounded fixture history"));
    projector
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
fn virtual_placeholders_resolve_colors_marks_and_left_inheritance() {
    let mut terminal = terminal();
    terminal.resize(8, 3, 16, 16).unwrap();
    terminal.vt_write(
        format!(
            "\x1b_Ga=T,f=24,s=4,v=4,i=42,p=9,c=2,r=2,U=1;{}\x1b\\",
            "/".repeat(64)
        )
        .as_bytes(),
    );
    terminal.vt_write("\x1b[38;2;0;0;42;58;2;0;0;9m\u{10eeee}\u{305}\u{305}\u{10eeee}\r\n\u{10eeee}\u{30d}\u{305}".as_bytes());
    let placements = project(&mut Images::default(), &terminal);
    // Upstream coalesces adjacent placeholders into a run per row instead
    // of the old phux projection's one rectangle per cell.
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].source, [0.0, 0.0, 4.0, 2.0]);
    assert_eq!(placements[1].source, [0.0, 2.0, 2.0, 2.0]);
    for placement in &placements {
        assert_eq!(placement.sample_source, [0.0, 0.0, 4.0, 4.0]);
    }
    assert_eq!((placements[0].cols, placements[0].rows), (2.0, 1.0));
    assert_eq!((placements[1].col, placements[1].row), (0.0, 1.0));
    assert!(placements.iter().all(|placement| placement.z == -1));
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
fn virtual_aspect_ratio_leaves_unused_cells_blank() {
    let mut terminal = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=3,r=1,U=1;/wAA/w==\x1b\\");
    terminal.vt_write("\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{10eeee}\u{10eeee}".as_bytes());
    let placements = project(&mut Images::default(), &terminal);
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].source, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(
        (placements[0].col, placements[0].cols, placements[0].rows),
        (0.5, 2.0, 1.0)
    );
}

#[test]
fn virtual_default_grid_and_placement_follow_engine_preference() {
    let mut terminal = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,U=1;/wAA/w==\x1b\\");
    terminal.vt_write("\x1b[38;5;42m\u{10eeee}\u{305}\u{305}".as_bytes());
    let mut images = Images::default();
    let natural = project(&mut images, &terminal);
    assert_eq!(natural.len(), 1);
    assert_eq!((natural[0].cols, natural[0].rows), (1.0, 0.5));
    terminal.vt_write(
        b"\x1b_Ga=p,i=42,p=9,U=1,c=2,r=2,z=3;\x1b\\\x1b_Ga=p,i=42,p=2,U=1,c=1,r=1,z=5;\x1b\\",
    );
    let preferred = project(&mut images, &terminal);
    assert_eq!(
        preferred.len(),
        1,
        "one cell must select one virtual prototype"
    );
    assert_eq!(
        preferred[0].z, -1,
        "upstream virtual placements are below text"
    );
    assert_eq!(preferred[0].rows, 0.5);
    assert_eq!(preferred[0].row, 0.25);
    terminal.vt_write(b"\x1b_Ga=d,d=i,i=42,p=2;\x1b\\");
    let next = project(&mut images, &terminal);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].z, -1);
    assert_eq!(
        next[0].row, 0.5,
        "the remaining 2x2 prototype centers its image differently"
    );
}

#[test]
fn placeholder_only_edits_reuse_the_single_render_snapshot() {
    let mut terminal = terminal();
    let mut images = Images::default();
    let mut projector = GridProjector::new().unwrap();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,U=1;/wAA/w==\x1b\\");
    terminal.vt_write("\x1b[38;5;42m\u{10eeee}\u{305}\u{305}".as_bytes());
    let first = project_cached(&mut images, &mut projector, &terminal);
    assert_eq!(first.len(), 1);
    terminal.vt_write(b"\r ");
    assert!(project_cached(&mut images, &mut projector, &terminal).is_empty());
    terminal.vt_write("\r\u{10eeee}\u{305}\u{305}".as_bytes());
    let restored = project_cached(&mut images, &mut projector, &terminal);
    assert_eq!(restored.len(), 1);
    assert!(Arc::ptr_eq(&first[0].image, &restored[0].image));
}

#[test]
fn relative_images_are_resolved_by_the_engine() {
    let mut terminal = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,p=9,c=2,r=2,C=1;/wAA/w==\x1b\\");
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=43,P=42,Q=9,H=1,V=1,C=1;AAD//w==\x1b\\");
    let images = project(&mut Images::default(), &terminal);
    let child = images
        .iter()
        .find(|image| *image.image.rgba == [0, 0, 255, 255])
        .unwrap();
    assert_eq!((child.col, child.row), (1.0, 1.0));
}

#[test]
fn placeholders_below_the_viewport_are_in_the_smooth_scroll_fringe() {
    let mut terminal = terminal();
    terminal.vt_write(b"a\r\nb\r\nc\r\nd");
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,U=1;/wAA/w==\x1b\\");
    terminal.vt_write("\x1b[3;1H\x1b[38;5;42m\u{10eeee}\u{305}\u{305}".as_bytes());
    terminal.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Top);
    let images = project(&mut Images::default(), &terminal);
    assert_eq!(images.len(), 1);
    assert!(
        images[0].row >= 3.0,
        "only overscan contains the placeholder"
    );
}

#[test]
fn classic_offsets_are_clamped_before_visibility_culling() {
    let mut terminal = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1,X=100,Y=100;/wAA/w==\x1b\\");
    let mut images = Images::default();
    let clamped = project(&mut images, &terminal);
    assert_eq!((clamped[0].col, clamped[0].row), (0.875, 0.9375));
    terminal.vt_write(b"\x1b_Ga=d,d=A;\x1b\\\x1b[H\x1b_Ga=T,f=24,s=1,v=8,i=43,C=1,Y=12;/wAA/wAA/wAA/wAA/wAA/wAA/wAA/wAA\x1b\\");
    terminal.vt_write(b"\x1b[3;1H\n");
    let visible = project(&mut images, &terminal);
    assert_eq!(visible.len(), 1);
    assert_eq!((visible[0].row, visible[0].rows), (-0.25, 0.5));
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
